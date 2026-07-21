//! Stable fingerprints for conversion settings.
//!
//! The fingerprint covers every setting that can alter conversion output. It
//! uses explicit field names and enum encodings so the digest stays independent
//! of Rust struct layout, declaration order, serde output, or debug formatting.

use sha2::{Digest, Sha256};

use crate::enums::{
    AacProfile, AudioCodec, AudioFormat, BitDepthTarget, DitherType, DsdFilterPreset, DsdLowpassMethod,
    DsdNoiseShaper, DsdToPcmGainMode, GainCompensation, ModulatorOrder, Mp3Mode,
    NyquistTransition, OpusContentType,
    PcmBitDepth, PreferredTool, RateTarget, ReplayGainMode, ResampleQuality, SoxSincPhase,
    SsrcProfile, WavPackMode,
};
use crate::dsd_reference::{
    DbNano, DsdInputFrontEnd, DsdReferencePlanSummary, DsdSourceKind, Sha256Digest,
};
use crate::source::{SourceInfo, SourceRepresentationKind};
use crate::settings::{
    AacSettings, DsdSettings, FlacSettings, MetadataSettings, Mp3Settings, OpusSettings,
    PipelineSettings, ReplayGainSettings, SincFilterSettings, SoxResamplerSettings,
    SoxrResamplerSettings, SsrcSettings, TrellisSettings, VerificationSettings, WavPackSettings,
};

/// Deterministic SHA-256 digest for [`PipelineSettings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SettingsFingerprint([u8; 32]);

impl SettingsFingerprint {
    /// Returns the raw 32-byte SHA-256 digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Returns the digest as lowercase hexadecimal text.
    #[must_use]
    pub fn to_hex(self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            push_hex_byte(&mut out, byte);
        }
        out
    }
}

impl std::fmt::Display for SettingsFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}


/// Canonical field-path inventory covered by [`settings_fingerprint`].
///
/// The list is public so integration tests can compare handoff, legacy, and
/// mutation coverage against the same conversion-affecting field set. Some
/// paths are mode-scoped: they are emitted only when the selected mode makes
/// them output-affecting.
pub const SETTINGS_FINGERPRINT_FIELD_PATHS: &[&str] = &[
    "target_format",
    "target_sample_rate",
    "target_bit_depth",
    "resample_quality",
    "nyquist_transition",
    "dither_type",
    "preferred_tool",
    "force_encode",
    "flac.compression_level",
    "flac.verify",
    "flac.write_md5",
    "mp3.mode",
    "mp3.bitrate_kbps",
    "mp3.vbr_quality",
    "aac.profile",
    "aac.bitrate_kbps",
    "opus.content_type",
    "opus.bitrate_kbps",
    "opus.complexity",
    "wavpack.mode",
    "wavpack.hybrid",
    "wavpack.hybrid_bitrate_kbps",
    "wavpack.correction_file",
    "ssrc.force",
    "ssrc.insane_mode",
    "ssrc.profile",
    "ssrc.attenuation_db",
    "ssrc.min_phase",
    "ssrc.dither_id",
    "ssrc.pdf_type",
    "sox_resampler.chebyshev",
    "sox_resampler.bandwidth_pct",
    "sox_resampler.phase",
    "sox_resampler.allow_aliasing",
    "sox_resampler.sinc_taps",
    "sox_resampler.sinc_attenuation_db",
    "sox_resampler.sinc_passband_hz",
    "sox_resampler.sinc_transition_hz",
    "sox_resampler.sinc_kaiser_beta",
    "sox_resampler.sinc_phase",
    "soxr_resampler.chebyshev",
    "soxr_resampler.cutoff",
    "soxr_resampler.phase",
    "dsd.noise_shaper",
    "dsd.modulator_order",
    "dsd.trellis",
    "dsd.trellis.lookahead",
    "dsd.trellis.nodes",
    "dsd.trellis.latency",
    "dsd.pcm_to_dsd_filter",
    "dsd.dsd_to_pcm_lowpass",
    "dsd.dsd_to_pcm_gain_mode",
    "dsd.dsd_to_pcm_auto_gain_margin_db",
    "dsd.dsd_to_pcm_gain_db",
    "dsd.sinc.oversample_factor",
    "dsd.sinc.taps",
    "dsd.sinc.passband_hz",
    "dsd.sinc.transition_hz",
    "dsd.sinc.kaiser_beta",
    "dsd.sinc.linear_phase",
    "dsd.sinc.allow_aliasing",
    "dsd.gain_compensation",
    "metadata.transfer_tags",
    "metadata.preserve_artwork",
    "metadata.store_source_audio_md5",
    "verification.verify_after_encode",
    "verification.prefer_native_flac_verify",
    "replay_gain.mode",
    "replay_gain.prevent_clipping",
    "replay_gain.existing_tags",
];

/// Number of conversion-affecting field paths in [`SETTINGS_FINGERPRINT_FIELD_PATHS`].
pub const SETTINGS_FINGERPRINT_FIELD_COUNT: usize = SETTINGS_FINGERPRINT_FIELD_PATHS.len();

/// Native-v2 DSD settings paths written by [`settings_snapshot_fingerprint_v2`].
///
/// This inventory is deliberately separate from [`SETTINGS_FINGERPRINT_FIELD_PATHS`],
/// which is frozen as legacy manifest-v1 authority. Additive directional DSD
/// settings must never be inferred into an old fingerprint domain.
pub const SETTINGS_SNAPSHOT_V2_DSD_FIELD_PATHS: &[&str] = &[
    "dsd.schema",
    "dsd.pcm_to_dsd.noise_shaper",
    "dsd.pcm_to_dsd.modulator_order",
    "dsd.pcm_to_dsd.trellis",
    "dsd.pcm_to_dsd.filter",
    "dsd.pcm_to_dsd.sinc.oversample_factor",
    "dsd.pcm_to_dsd.sinc.taps",
    "dsd.pcm_to_dsd.sinc.passband_hz",
    "dsd.pcm_to_dsd.sinc.transition_hz",
    "dsd.pcm_to_dsd.sinc.kaiser_beta",
    "dsd.pcm_to_dsd.sinc.linear_phase",
    "dsd.pcm_to_dsd.sinc.allow_aliasing",
    "dsd.pcm_to_dsd.gain_compensation",
    "dsd.from_dsd.pathway",
    "dsd.from_dsd.reference_policy",
    "dsd.from_dsd.profile",
    "dsd.from_dsd.gain_mode",
    "dsd.from_dsd.fixed_gain_db",
    "dsd.from_dsd.normalize_peak_target_dbfs",
];

/// Number of native-v2 directional DSD fields in the settings snapshot.
pub const SETTINGS_SNAPSHOT_V2_DSD_FIELD_COUNT: usize =
    SETTINGS_SNAPSHOT_V2_DSD_FIELD_PATHS.len();

/// Returns a deterministic content fingerprint for all conversion-affecting
/// fields in [`PipelineSettings`].
#[must_use]
pub fn settings_fingerprint(settings: &PipelineSettings) -> SettingsFingerprint {
    let mut writer = FingerprintWriter::new();
    writer.field_static("schema", "tonepoet-pipeline-settings-fingerprint/v1");
    push_pipeline_settings(&mut writer, settings);
    SettingsFingerprint(writer.finish())
}

/// Frozen name for the byte-for-byte legacy settings identity.
pub type LegacySettingsFingerprintV1 = SettingsFingerprint;

/// Return the exact pre-directional settings fingerprint used by manifest v1.
#[must_use]
pub fn legacy_settings_fingerprint_v1(
    settings: &PipelineSettings,
) -> LegacySettingsFingerprintV1 {
    settings_fingerprint(settings)
}

/// Canonical native-v2 settings snapshot digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SettingsSnapshotFingerprintV2(pub Sha256Digest);

/// Source-aware Reference behavior digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BehaviorFingerprintV1(pub Sha256Digest);

/// Runtime/tool closure digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ExecutionFingerprintV1(pub Sha256Digest);

/// Path-normalized immutable plan digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SemanticPlanHashV1(pub Sha256Digest);

/// Exact identity of one policy-owned metadata mutator.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct ReferenceMetadataMutatorIdentityInput {
    /// Canonical resolved executable path.
    pub canonical_path: String,
    /// SHA-256 of the executable bytes.
    pub executable_sha256: Sha256Digest,
    /// Exact version string the tool reported at attestation.
    pub reported_version: String,
    /// Package/store closure digest binding the complete toolchain.
    pub closure_digest: Sha256Digest,
}

/// Exact metadata-mutator closure admitted for a Reference execution.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct ReferenceMetadataMutatorToolchainInput {
    /// FLAC tag mutator identity (metaflac).
    pub metaflac: ReferenceMetadataMutatorIdentityInput,
    /// WavPack tag mutator identity (wvtag).
    pub wvtag: ReferenceMetadataMutatorIdentityInput,
    /// M4A freeform tag mutator identity (AtomicParsley).
    pub atomic_parsley: ReferenceMetadataMutatorIdentityInput,
}

/// Exact tool/runtime closure inputs for native-v2 execution identity.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct ReferenceExecutionIdentityInput {
    /// Stable planner/build identity.
    pub planner_build_identity: String,
    /// Platform ABI identity.
    pub platform_abi_digest: Sha256Digest,
    /// Runtime CPU/dispatch identity.
    pub runtime_dispatch_digest: Sha256Digest,
    /// Exact SoX-ng executable content digest.
    pub sox_ng_sha256: Sha256Digest,
    /// Reported SoX-ng version text.
    pub sox_ng_version: String,
    /// Package/store closure identity for SoX-ng.
    pub sox_ng_closure_digest: Sha256Digest,
    /// Qualified SoX-ng behavior-probe identity.
    pub sox_ng_behavior_probe_digest: Sha256Digest,
    /// Exact FFmpeg executable content digest.
    pub ffmpeg_sha256: Sha256Digest,
    /// Reported FFmpeg version text.
    pub ffmpeg_version: String,
    /// Package/store closure identity for FFmpeg.
    pub ffmpeg_closure_digest: Sha256Digest,
    /// Qualified FFmpeg behavior-probe identity.
    pub ffmpeg_behavior_probe_digest: Sha256Digest,
    /// Exact policy-owned metadata mutators when metadata mutation is enabled.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
    pub metadata_mutators: Option<ReferenceMetadataMutatorToolchainInput>,
    /// In-process DST/SACD build identity.
    pub sacd_rs_build_identity: String,
    /// Pinned byte-exact DST fixture corpus.
    pub dst_fixture_digest: Sha256Digest,
    /// Frozen analyzer reporting uncertainty.
    pub reporting_uncertainty: DbNano,
    /// Frozen analyzer residual bound.
    pub analyzer_residual: DbNano,
}

/// Hash the canonical native-v2 settings snapshot. This is audit identity, not
/// sufficient rerun authority by itself.
#[must_use]
pub fn settings_snapshot_fingerprint_v2(
    settings: &PipelineSettings,
) -> SettingsSnapshotFingerprintV2 {
    let mut writer = FingerprintWriter::new();
    writer.field_static("schema", "tonepoet-settings-snapshot/v2");
    push_pipeline_settings_v2(&mut writer, settings);
    SettingsSnapshotFingerprintV2(Sha256Digest(writer.finish()))
}

/// Hash the source-aware, pathway-scoped Reference behavior.
#[must_use]
pub fn conversion_behavior_fingerprint_v1(
    summary: &DsdReferencePlanSummary,
    source_kind: &DsdSourceKind,
) -> BehaviorFingerprintV1 {
    let mut writer = FingerprintWriter::new();
    writer.field_static("schema", "tonepoet-dsd-reference-behavior/v1");
    writer.field_static("policy", summary.policy.key());
    writer.field_static("target", summary.target.key());
    writer.field_static("profile", summary.profile.key());
    writer.field_string("front_end", canonical_front_end(summary.front_end));
    writer.field_string("source_kind", canonical_source_kind(source_kind));
    writer.field_string("final.sample_rate_hz", summary.final_pcm.sample_rate_hz.to_string());
    writer.field_string("final.channels", summary.final_pcm.channels.to_string());
    writer.field_static("final.sample_kind", sample_kind(summary.final_pcm.sample_kind));
    writer.field_static("final.bit_depth", pcm_bit_depth(summary.final_pcm.bit_depth));
    writer.field_string("gain_policy", canonical_gain_policy(summary.gain_policy));
    writer.field_string(
        "package_compression_level",
        summary
            .package_compression_level
            .map_or_else(|| "none".to_string(), |value| value.to_string()),
    );
    BehaviorFingerprintV1(Sha256Digest(writer.finish()))
}

/// Bind behavior and semantic plan to the exact runtime/tool closure.
#[must_use]
pub fn execution_fingerprint_v1(
    behavior: BehaviorFingerprintV1,
    semantic_plan: SemanticPlanHashV1,
    qualification_manifest_digest: Sha256Digest,
    identity: &ReferenceExecutionIdentityInput,
) -> ExecutionFingerprintV1 {
    let mut writer = FingerprintWriter::new();
    writer.field_static("schema", "tonepoet-dsd-reference-execution/v1");
    writer.field_static("behavior", &behavior.0.to_hex());
    writer.field_static("semantic_plan", &semantic_plan.0.to_hex());
    writer.field_static("qualification", &qualification_manifest_digest.to_hex());
    writer.field_static("planner_build", &identity.planner_build_identity);
    writer.field_static("platform_abi", &identity.platform_abi_digest.to_hex());
    writer.field_static("runtime_dispatch", &identity.runtime_dispatch_digest.to_hex());
    writer.field_static("sox_ng_sha256", &identity.sox_ng_sha256.to_hex());
    writer.field_static("sox_ng_version", &identity.sox_ng_version);
    writer.field_static("sox_ng_closure", &identity.sox_ng_closure_digest.to_hex());
    writer.field_static(
        "sox_ng_behavior_probe",
        &identity.sox_ng_behavior_probe_digest.to_hex(),
    );
    writer.field_static("ffmpeg_sha256", &identity.ffmpeg_sha256.to_hex());
    writer.field_static("ffmpeg_version", &identity.ffmpeg_version);
    writer.field_static("ffmpeg_closure", &identity.ffmpeg_closure_digest.to_hex());
    writer.field_static(
        "ffmpeg_behavior_probe",
        &identity.ffmpeg_behavior_probe_digest.to_hex(),
    );
    if let Some(mutators) = &identity.metadata_mutators {
        for (name, mutator) in [
            ("metaflac", &mutators.metaflac),
            ("wvtag", &mutators.wvtag),
            ("atomic_parsley", &mutators.atomic_parsley),
        ] {
            writer.field_string(
                &format!("metadata_mutator.{name}.canonical_path"),
                mutator.canonical_path.clone(),
            );
            writer.field_string(
                &format!("metadata_mutator.{name}.sha256"),
                mutator.executable_sha256.to_hex(),
            );
            writer.field_string(
                &format!("metadata_mutator.{name}.version"),
                mutator.reported_version.clone(),
            );
            writer.field_string(
                &format!("metadata_mutator.{name}.closure"),
                mutator.closure_digest.to_hex(),
            );
        }
    }
    writer.field_static("sacd_rs_build", &identity.sacd_rs_build_identity);
    writer.field_static("dst_fixture", &identity.dst_fixture_digest.to_hex());
    writer.field_static(
        "analyzer_reporting_uncertainty_db",
        &identity.reporting_uncertainty.render(false),
    );
    writer.field_static(
        "analyzer_residual_db",
        &identity.analyzer_residual.render(false),
    );
    ExecutionFingerprintV1(Sha256Digest(writer.finish()))
}

/// Hash the exact source probe facts that can affect Reference planning.
/// Paths, timestamps, duration estimates, and mutable tag metadata are excluded.
#[must_use]
pub fn reference_source_probe_digest_v1(source: &SourceInfo) -> Sha256Digest {
    let mut writer = FingerprintWriter::new();
    writer.field_static("schema", "tonepoet-dsd-reference-source-probe/v1");
    writer.field_string("format", audio_format(&source.format));
    writer.field_string("codec", audio_codec(&source.codec));
    writer.field_string(
        "sample_rate_hz",
        source.sample_rate_hz.map_or_else(|| "none".to_string(), |value| value.to_string()),
    );
    writer.field_string(
        "bit_depth",
        source.bit_depth.map_or_else(|| "none".to_string(), |value| pcm_bit_depth(value).to_string()),
    );
    writer.field_string(
        "true_source_depth",
        source.true_source_depth.map_or_else(|| "none".to_string(), |value| pcm_bit_depth(value).to_string()),
    );
    writer.field_static(
        "source_representation",
        match source.source_representation {
            SourceRepresentationKind::Pcm => "pcm",
            SourceRepresentationKind::Dsd => "dsd",
            SourceRepresentationKind::Lossy => "lossy",
            SourceRepresentationKind::Unknown => "unknown",
            SourceRepresentationKind::Unspecified => "unspecified",
        },
    );
    writer.field_string(
        "sample_kind",
        source.sample_kind.map_or_else(|| "none".to_string(), |value| sample_kind(value).to_string()),
    );
    writer.field_string(
        "channels",
        source.channels.map_or_else(|| "none".to_string(), |value| value.to_string()),
    );
    writer.field_string(
        "dsd_source_kind",
        source.dsd_source_kind.as_ref().map_or_else(
            || "none".to_string(),
            canonical_source_kind,
        ),
    );
    Sha256Digest(writer.finish())
}

struct FingerprintWriter {
    hasher: Sha256,
}

impl FingerprintWriter {
    fn new() -> Self {
        Self {
            hasher: Sha256::new(),
        }
    }

    fn field_static(&mut self, path: &str, value: &str) {
        self.hasher.update(path.as_bytes());
        self.hasher.update(b"=");
        self.hasher.update(value.len().to_string().as_bytes());
        self.hasher.update(b":");
        self.hasher.update(value.as_bytes());
        self.hasher.update(b"\n");
    }

    fn field_string(&mut self, path: &str, value: String) {
        self.field_static(path, &value);
    }

    fn finish(self) -> [u8; 32] {
        self.hasher.finalize().into()
    }
}

fn push_hex_byte(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(HEX[(byte >> 4) as usize] as char);
    out.push(HEX[(byte & 0x0f) as usize] as char);
}

fn push_pipeline_settings(writer: &mut FingerprintWriter, settings: &PipelineSettings) {
    writer.field_string("target_format", audio_format(&settings.target_format));
    writer.field_string("target_sample_rate", rate_target(settings.target_sample_rate));
    writer.field_string("target_bit_depth", bit_depth_target(settings.target_bit_depth));
    writer.field_static("resample_quality", resample_quality(settings.resample_quality));
    writer.field_static(
        "nyquist_transition",
        nyquist_transition(settings.nyquist_transition),
    );
    writer.field_static("dither_type", dither_type(settings.dither_type));
    writer.field_string("preferred_tool", preferred_tool(&settings.preferred_tool));
    writer.field_static("force_encode", bool_value(settings.force_encode));
    push_flac(writer, &settings.flac);
    push_mp3(writer, &settings.mp3);
    push_aac(writer, &settings.aac);
    push_opus(writer, &settings.opus);
    push_wavpack(writer, &settings.wavpack);
    push_ssrc(writer, &settings.ssrc);
    push_sox_resampler(writer, &settings.sox_resampler);
    push_soxr_resampler(writer, &settings.soxr_resampler);
    push_dsd(writer, &settings.dsd);
    push_metadata(writer, &settings.metadata);
    push_verification(writer, &settings.verification);
    push_replay_gain(writer, &settings.replay_gain);
}

fn push_pipeline_settings_v2(writer: &mut FingerprintWriter, settings: &PipelineSettings) {
    writer.field_string("target_format", audio_format(&settings.target_format));
    writer.field_string("target_sample_rate", rate_target(settings.target_sample_rate));
    writer.field_string("target_bit_depth", bit_depth_target(settings.target_bit_depth));
    writer.field_static("resample_quality", resample_quality(settings.resample_quality));
    writer.field_static("nyquist_transition", nyquist_transition(settings.nyquist_transition));
    writer.field_static("dither_type", dither_type(settings.dither_type));
    writer.field_string("preferred_tool", preferred_tool(&settings.preferred_tool));
    writer.field_static("force_encode", bool_value(settings.force_encode));
    push_flac(writer, &settings.flac);
    push_mp3(writer, &settings.mp3);
    push_aac(writer, &settings.aac);
    push_opus(writer, &settings.opus);
    push_wavpack(writer, &settings.wavpack);
    push_ssrc(writer, &settings.ssrc);
    push_sox_resampler(writer, &settings.sox_resampler);
    push_soxr_resampler(writer, &settings.soxr_resampler);
    push_native_dsd_v2(writer, &settings.dsd);
    push_metadata(writer, &settings.metadata);
    push_verification(writer, &settings.verification);
    push_replay_gain(writer, &settings.replay_gain);
}

fn push_native_dsd_v2(writer: &mut FingerprintWriter, settings: &DsdSettings) {
    let pcm = settings.pcm_to_dsd;
    writer.field_static("dsd.schema", if settings.is_native_v2() { "native_v2" } else { "legacy_v1" });
    writer.field_static("dsd.pcm_to_dsd.noise_shaper", dsd_noise_shaper(pcm.noise_shaper));
    writer.field_static("dsd.pcm_to_dsd.modulator_order", modulator_order(pcm.modulator_order));
    writer.field_string("dsd.pcm_to_dsd.trellis", option_trellis(pcm.trellis));
    writer.field_static("dsd.pcm_to_dsd.filter", dsd_filter_preset(pcm.filter));
    push_sinc_v2(writer, &pcm.sinc);
    writer.field_string("dsd.pcm_to_dsd.gain_compensation", gain_compensation(pcm.gain_compensation));
    let from = settings.from_dsd;
    writer.field_static("dsd.from_dsd.pathway", match from.pathway {
        crate::DsdSourcePathway::Reference => "reference",
        crate::DsdSourcePathway::Manual => "manual",
    });
    writer.field_static("dsd.from_dsd.reference_policy", from.reference_policy.key());
    writer.field_static("dsd.from_dsd.profile", match from.profile {
        crate::DsdReconstructionSelection::Reference => "reference",
        crate::DsdReconstructionSelection::Wideband => "wideband",
    });
    writer.field_static("dsd.from_dsd.gain_mode", match from.gain_mode {
        crate::DsdSourceGainMode::Reference => "reference",
        crate::DsdSourceGainMode::NativeLevel => "native_level",
        crate::DsdSourceGainMode::Fixed => "fixed",
        crate::DsdSourceGainMode::NormalizePeak => "normalize_peak",
    });
    writer.field_string("dsd.from_dsd.fixed_gain_db", option_db_nano(from.fixed_gain_db));
    writer.field_string(
        "dsd.from_dsd.normalize_peak_target_dbfs",
        from.normalize_peak_target_dbfs.render(false),
    );
}

fn push_sinc_v2(writer: &mut FingerprintWriter, settings: &SincFilterSettings) {
    writer.field_string("dsd.pcm_to_dsd.sinc.oversample_factor", settings.oversample_factor.to_string());
    writer.field_string("dsd.pcm_to_dsd.sinc.taps", settings.taps.to_string());
    writer.field_string("dsd.pcm_to_dsd.sinc.passband_hz", f32_value(settings.passband_hz));
    writer.field_string("dsd.pcm_to_dsd.sinc.transition_hz", f32_value(settings.transition_hz));
    writer.field_string("dsd.pcm_to_dsd.sinc.kaiser_beta", f32_value(settings.kaiser_beta));
    writer.field_static("dsd.pcm_to_dsd.sinc.linear_phase", bool_value(settings.linear_phase));
    writer.field_static("dsd.pcm_to_dsd.sinc.allow_aliasing", bool_value(settings.allow_aliasing));
}

fn option_db_nano(value: Option<DbNano>) -> String {
    value.map_or_else(|| "None".to_string(), |value| format!("Some({})", value.render(false)))
}

fn sample_kind(value: crate::SampleKind) -> &'static str {
    match value {
        crate::SampleKind::SignedInteger => "signed_integer",
        crate::SampleKind::UnsignedInteger => "unsigned_integer",
        crate::SampleKind::Float => "float",
        crate::SampleKind::Dsd => "dsd",
    }
}

fn canonical_front_end(front_end: DsdInputFrontEnd) -> String {
    match front_end {
        DsdInputFrontEnd::NativeUncompressed => "native_uncompressed".to_string(),
        DsdInputFrontEnd::DsdiffDst { decoder } => format!("dsdiff_dst:{decoder:?}"),
        DsdInputFrontEnd::SacdDsd { extractor } => format!("sacd_dsd:{extractor:?}"),
        DsdInputFrontEnd::SacdDst { extractor, decoder } => {
            format!("sacd_dst:{extractor:?}:{decoder:?}")
        }
    }
}

fn canonical_source_kind(source: &DsdSourceKind) -> String {
    match source {
        DsdSourceKind::DsfUncompressed => "dsf_uncompressed".to_string(),
        DsdSourceKind::DsdiffUncompressed => "dsdiff_uncompressed".to_string(),
        DsdSourceKind::DsdiffDst => "dsdiff_dst".to_string(),
        DsdSourceKind::SacdTrack { frame_format, selection } => format!(
            "sacd:{frame_format:?}:{:?}:{}:{}:{}:{}",
            selection.area,
            selection.track_index_zero_based,
            selection.start_frame,
            selection.frame_count,
            selection.toc_digest.to_hex(),
        ),
        DsdSourceKind::UnknownDsdContainer => "unknown_dsd_container".to_string(),
    }
}

fn canonical_gain_policy(policy: crate::ResolvedGainPolicy) -> String {
    match policy {
        crate::ResolvedGainPolicy::ReferenceCompensated { requested_gain, ceiling, terminal_bound } => format!(
            "reference:{}:{}:{}:{}",
            requested_gain.render(false),
            ceiling.render(false),
            terminal_bound.max_added_peak_fs_q63_ceil,
            terminal_bound.derivation_digest.to_hex(),
        ),
        crate::ResolvedGainPolicy::NativeLevelExact { gain, ceiling, terminal_bound } => format!(
            "native:{}:{}:{}:{}",
            gain.render(false),
            ceiling.render(false),
            terminal_bound.max_added_peak_fs_q63_ceil,
            terminal_bound.derivation_digest.to_hex(),
        ),
        crate::ResolvedGainPolicy::FixedExact { gain, ceiling, terminal_bound } => format!(
            "fixed:{}:{}:{}:{}",
            gain.render(false),
            ceiling.render(false),
            terminal_bound.max_added_peak_fs_q63_ceil,
            terminal_bound.derivation_digest.to_hex(),
        ),
        crate::ResolvedGainPolicy::NormalizePeak { target_dbfs } => {
            format!("normalize:{}", target_dbfs.render(false))
        }
    }
}

fn push_flac(writer: &mut FingerprintWriter, settings: &FlacSettings) {
    writer.field_string("flac.compression_level", settings.compression_level.to_string());
    writer.field_static("flac.verify", bool_value(settings.verify));
    writer.field_static("flac.write_md5", bool_value(settings.write_md5));
}

fn push_mp3(writer: &mut FingerprintWriter, settings: &Mp3Settings) {
    writer.field_static("mp3.mode", mp3_mode(settings.mode));
    writer.field_string("mp3.bitrate_kbps", settings.bitrate_kbps.to_string());
    writer.field_string("mp3.vbr_quality", settings.vbr_quality.to_string());
}

fn push_aac(writer: &mut FingerprintWriter, settings: &AacSettings) {
    writer.field_static("aac.profile", aac_profile(settings.profile));
    writer.field_string("aac.bitrate_kbps", settings.bitrate_kbps.to_string());
}

fn push_opus(writer: &mut FingerprintWriter, settings: &OpusSettings) {
    writer.field_static("opus.content_type", opus_content_type(settings.content_type));
    writer.field_string("opus.bitrate_kbps", settings.bitrate_kbps.to_string());
    writer.field_string("opus.complexity", settings.complexity.to_string());
}

fn push_wavpack(writer: &mut FingerprintWriter, settings: &WavPackSettings) {
    writer.field_static("wavpack.mode", wavpack_mode(settings.mode));
    writer.field_static("wavpack.hybrid", bool_value(settings.hybrid));
    writer.field_string(
        "wavpack.hybrid_bitrate_kbps",
        settings.hybrid_bitrate_kbps.to_string(),
    );
    writer.field_static("wavpack.correction_file", bool_value(settings.correction_file));
}

fn push_ssrc(writer: &mut FingerprintWriter, settings: &SsrcSettings) {
    writer.field_static("ssrc.force", bool_value(settings.force));
    writer.field_static("ssrc.insane_mode", bool_value(settings.insane_mode));
    writer.field_string("ssrc.profile", option_static(settings.profile.map(ssrc_profile)));
    writer.field_string("ssrc.attenuation_db", option_f32(settings.attenuation_db));
    writer.field_static("ssrc.min_phase", bool_value(settings.min_phase));
    writer.field_string("ssrc.dither_id", option_u8(settings.dither_id));
    writer.field_string(
        "ssrc.pdf_type",
        option_static(settings.pdf_type.map(ssrc_pdf_type)),
    );
}

fn ssrc_pdf_type(pdf: crate::enums::SsrcPdfType) -> &'static str {
    match pdf {
        crate::enums::SsrcPdfType::Rectangular => "Rectangular",
        crate::enums::SsrcPdfType::Triangular => "Triangular",
    }
}

fn push_sox_resampler(writer: &mut FingerprintWriter, settings: &SoxResamplerSettings) {
    writer.field_static("sox_resampler.chebyshev", bool_value(settings.chebyshev));
    writer.field_string(
        "sox_resampler.bandwidth_pct",
        option_f32(settings.bandwidth_pct),
    );
    writer.field_string("sox_resampler.phase", option_u8(settings.phase));
    writer.field_static(
        "sox_resampler.allow_aliasing",
        bool_value(settings.allow_aliasing),
    );
    writer.field_string(
        "sox_resampler.sinc_taps",
        settings.sinc_taps.map(|v| v.to_string()).unwrap_or_else(|| "None".to_string()),
    );
    writer.field_string(
        "sox_resampler.sinc_attenuation_db",
        settings.sinc_attenuation_db.map(|v| v.to_string()).unwrap_or_else(|| "None".to_string()),
    );
    writer.field_string("sox_resampler.sinc_passband_hz", option_f32(settings.sinc_passband_hz));
    writer.field_string("sox_resampler.sinc_transition_hz", option_f32(settings.sinc_transition_hz));
    writer.field_string("sox_resampler.sinc_kaiser_beta", option_f32(settings.sinc_kaiser_beta));
    writer.field_static(
        "sox_resampler.sinc_phase",
        match settings.sinc_phase {
            Some(SoxSincPhase::Linear) => "Linear",
            Some(SoxSincPhase::Minimum) => "Minimum",
            Some(SoxSincPhase::Intermediate) => "Intermediate",
            None => "None",
        },
    );
}

fn push_soxr_resampler(writer: &mut FingerprintWriter, settings: &SoxrResamplerSettings) {
    writer.field_static("soxr_resampler.chebyshev", bool_value(settings.chebyshev));
    writer.field_string("soxr_resampler.cutoff", option_f32(settings.cutoff));
    writer.field_string("soxr_resampler.phase", option_u8(settings.phase));
}

fn option_u8(value: Option<u8>) -> String {
    match value {
        Some(v) => v.to_string(),
        None => "None".to_string(),
    }
}

fn push_dsd(writer: &mut FingerprintWriter, settings: &DsdSettings) {
    let pcm = settings.pcm_to_dsd;
    let legacy = settings.legacy_compat_wire();
    writer.field_static("dsd.noise_shaper", dsd_noise_shaper(pcm.noise_shaper));
    writer.field_static("dsd.modulator_order", modulator_order(pcm.modulator_order));
    writer.field_string("dsd.trellis", option_trellis(pcm.trellis));
    if let Some(trellis) = pcm.trellis {
        writer.field_string("dsd.trellis.lookahead", trellis.lookahead.to_string());
        writer.field_string("dsd.trellis.nodes", trellis.nodes.to_string());
        writer.field_string("dsd.trellis.latency", option_u16(trellis.latency));
    } else {
        writer.field_static("dsd.trellis.lookahead", "None");
        writer.field_static("dsd.trellis.nodes", "None");
        writer.field_static("dsd.trellis.latency", "None");
    }
    writer.field_static("dsd.pcm_to_dsd_filter", dsd_filter_preset(pcm.filter));
    writer.field_static(
        "dsd.dsd_to_pcm_lowpass",
        dsd_lowpass_method(legacy.dsd_to_pcm_lowpass),
    );
    push_legacy_dsd_to_pcm_gain(writer, legacy);
    push_sinc(writer, &pcm.sinc);
    writer.field_string(
        "dsd.gain_compensation",
        gain_compensation(pcm.gain_compensation),
    );
}

fn push_legacy_dsd_to_pcm_gain(
    writer: &mut FingerprintWriter,
    settings: crate::settings::LegacyDsdSettingsWireV1,
) {
    writer.field_static(
        "dsd.dsd_to_pcm_gain_mode",
        dsd_to_pcm_gain_mode(settings.dsd_to_pcm_gain_mode),
    );

    match settings.dsd_to_pcm_gain_mode {
        DsdToPcmGainMode::Disabled => {
            if let Some(gain_db) = settings.dsd_to_pcm_gain_db {
                writer.field_string("dsd.dsd_to_pcm_gain_db", option_f32(Some(gain_db)));
            }
        }
        DsdToPcmGainMode::Auto => {
            writer.field_string(
                "dsd.dsd_to_pcm_auto_gain_margin_db",
                f32_value(settings.dsd_to_pcm_auto_gain_margin_db),
            );
        }
        DsdToPcmGainMode::Manual => {
            writer.field_string(
                "dsd.dsd_to_pcm_gain_db",
                option_f32(settings.dsd_to_pcm_gain_db),
            );
        }
    }
}

fn push_sinc(writer: &mut FingerprintWriter, settings: &SincFilterSettings) {
    writer.field_string(
        "dsd.sinc.oversample_factor",
        settings.oversample_factor.to_string(),
    );
    writer.field_string("dsd.sinc.taps", settings.taps.to_string());
    writer.field_string("dsd.sinc.passband_hz", f32_value(settings.passband_hz));
    writer.field_string("dsd.sinc.transition_hz", f32_value(settings.transition_hz));
    writer.field_string("dsd.sinc.kaiser_beta", f32_value(settings.kaiser_beta));
    writer.field_static("dsd.sinc.linear_phase", bool_value(settings.linear_phase));
    writer.field_static("dsd.sinc.allow_aliasing", bool_value(settings.allow_aliasing));
}

fn push_metadata(writer: &mut FingerprintWriter, settings: &MetadataSettings) {
    writer.field_static("metadata.transfer_tags", bool_value(settings.transfer_tags));
    writer.field_static("metadata.preserve_artwork", bool_value(settings.preserve_artwork));
    writer.field_static(
        "metadata.store_source_audio_md5",
        bool_value(settings.store_source_audio_md5),
    );
}

fn push_verification(writer: &mut FingerprintWriter, settings: &VerificationSettings) {
    writer.field_static(
        "verification.verify_after_encode",
        bool_value(settings.verify_after_encode),
    );
    writer.field_static(
        "verification.prefer_native_flac_verify",
        bool_value(settings.prefer_native_flac_verify),
    );
}

fn push_replay_gain(writer: &mut FingerprintWriter, settings: &ReplayGainSettings) {
    writer.field_string(
        "replay_gain.mode",
        option_static(settings.mode.map(replay_gain_mode)),
    );
    writer.field_static(
        "replay_gain.prevent_clipping",
        bool_value(settings.prevent_clipping),
    );
    writer.field_static(
        "replay_gain.existing_tags",
        match settings.existing_tags {
            crate::ReplayGainExistingTagPolicy::Rescan => "rescan",
            crate::ReplayGainExistingTagPolicy::SkipIfComplete => "skip_if_complete",
        },
    );
}

fn bool_value(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn audio_format(value: &AudioFormat) -> String {
    match value {
        AudioFormat::Flac => "Flac".to_string(),
        AudioFormat::Wav => "Wav".to_string(),
        AudioFormat::Aiff => "Aiff".to_string(),
        AudioFormat::WavPack => "WavPack".to_string(),
        AudioFormat::Mp3 => "Mp3".to_string(),
        AudioFormat::Aac => "Aac".to_string(),
        AudioFormat::Opus => "Opus".to_string(),
        AudioFormat::Alac => "Alac".to_string(),
        AudioFormat::Dsf => "Dsf".to_string(),
        AudioFormat::Dff => "Dff".to_string(),
        AudioFormat::Dts => "Dts".to_string(),
        AudioFormat::Ac3 => "Ac3".to_string(),
        AudioFormat::Custom {
            extension,
            display_name,
        } => format!(
            "Custom(extension={},display_name={})",
            string_value(extension),
            string_value(display_name)
        ),
    }
}

fn audio_codec(value: &AudioCodec) -> String {
    match value {
        AudioCodec::Flac => "flac".to_string(),
        AudioCodec::PcmSigned => "pcm_signed".to_string(),
        AudioCodec::PcmUnsigned => "pcm_unsigned".to_string(),
        AudioCodec::PcmFloat => "pcm_float".to_string(),
        AudioCodec::WavPack => "wavpack".to_string(),
        AudioCodec::Mp3 => "mp3".to_string(),
        AudioCodec::Aac => "aac".to_string(),
        AudioCodec::Opus => "opus".to_string(),
        AudioCodec::Alac => "alac".to_string(),
        AudioCodec::Dsd => "dsd".to_string(),
        AudioCodec::Custom(name) => format!("custom({})", string_value(name)),
    }
}

fn string_value(value: &str) -> String {
    format!("{}:{value}", value.len())
}

fn rate_target(value: RateTarget) -> String {
    match value {
        RateTarget::Source => "Source".to_string(),
        RateTarget::PcmHz(hz) => format!("PcmHz({hz})"),
        RateTarget::Dsd(rate) => format!("Dsd({})", dsd_rate(rate)),
    }
}

fn bit_depth_target(value: BitDepthTarget) -> String {
    match value {
        BitDepthTarget::Source => "Source".to_string(),
        BitDepthTarget::Pcm(depth) => format!("Pcm({})", pcm_bit_depth(depth)),
    }
}

fn preferred_tool(value: &PreferredTool) -> String {
    match value {
        PreferredTool::Auto => "Auto".to_string(),
        PreferredTool::Ffmpeg => "Ffmpeg".to_string(),
        PreferredTool::Sox => "Sox".to_string(),
        PreferredTool::Ssrc => "Ssrc".to_string(),
        PreferredTool::Custom(name) => format!("Custom({})", string_value(name)),
    }
}

fn option_static(value: Option<&'static str>) -> String {
    match value {
        Some(value) => format!("Some({value})"),
        None => "None".to_string(),
    }
}

fn option_u16(value: Option<u16>) -> String {
    match value {
        Some(value) => format!("Some({value})"),
        None => "None".to_string(),
    }
}

fn option_f32(value: Option<f32>) -> String {
    match value {
        Some(value) => format!("Some({})", f32_value(value)),
        None => "None".to_string(),
    }
}

fn option_trellis(value: Option<TrellisSettings>) -> String {
    match value {
        Some(_) => "Some".to_string(),
        None => "None".to_string(),
    }
}

fn f32_value(value: f32) -> String {
    format!("f32bits:{:08x}", value.to_bits())
}

fn resample_quality(value: ResampleQuality) -> &'static str {
    match value {
        ResampleQuality::Low => "Low",
        ResampleQuality::Medium => "Medium",
        ResampleQuality::High => "High",
        ResampleQuality::VeryHigh => "VeryHigh",
        ResampleQuality::Ultra => "Ultra",
        ResampleQuality::Insane => "Insane",
    }
}

fn nyquist_transition(value: NyquistTransition) -> &'static str {
    match value {
        NyquistTransition::Gentle => "Gentle",
        NyquistTransition::Medium => "Medium",
        NyquistTransition::Steep => "Steep",
        NyquistTransition::Sharp => "Sharp",
        NyquistTransition::BrickWall => "BrickWall",
    }
}

fn dither_type(value: DitherType) -> &'static str {
    match value {
        DitherType::None => "None",
        DitherType::Tpdf => "Tpdf",
        DitherType::SlopedTpdf => "SlopedTpdf",
        DitherType::Shibata => "Shibata",
        DitherType::Lipshitz => "Lipshitz",
        DitherType::FWeighted => "FWeighted",
        DitherType::ModifiedEWeighted => "ModifiedEWeighted",
        DitherType::ImprovedEWeighted => "ImprovedEWeighted",
        DitherType::Gesemann => "Gesemann",
        DitherType::LowShibata => "LowShibata",
        DitherType::HighShibata => "HighShibata",
    }
}

fn mp3_mode(value: Mp3Mode) -> &'static str {
    match value {
        Mp3Mode::Cbr => "Cbr",
        Mp3Mode::Vbr => "Vbr",
        Mp3Mode::Abr => "Abr",
    }
}

fn aac_profile(value: AacProfile) -> &'static str {
    match value {
        AacProfile::LcAac => "LcAac",
        AacProfile::HeAac => "HeAac",
        AacProfile::HeAacV2 => "HeAacV2",
    }
}

fn replay_gain_mode(value: ReplayGainMode) -> &'static str {
    match value {
        ReplayGainMode::Track => "Track",
        ReplayGainMode::Album => "Album",
        ReplayGainMode::Both => "Both",
    }
}

fn opus_content_type(value: OpusContentType) -> &'static str {
    match value {
        OpusContentType::Auto => "Auto",
        OpusContentType::Music => "Music",
        OpusContentType::Speech => "Speech",
    }
}

fn wavpack_mode(value: WavPackMode) -> &'static str {
    match value {
        WavPackMode::Normal => "Normal",
        WavPackMode::Fast => "Fast",
        WavPackMode::High => "High",
        WavPackMode::VeryHigh => "VeryHigh",
    }
}

fn ssrc_profile(value: SsrcProfile) -> &'static str {
    match value {
        SsrcProfile::Insane => "Insane",
        SsrcProfile::High => "High",
        SsrcProfile::Long => "Long",
        SsrcProfile::Standard => "Standard",
        SsrcProfile::Short => "Short",
        SsrcProfile::Fast => "Fast",
        SsrcProfile::Lightning => "Lightning",
    }
}

fn dsd_noise_shaper(value: DsdNoiseShaper) -> &'static str {
    match value {
        DsdNoiseShaper::Clans => "Clans",
        DsdNoiseShaper::Sdm => "Sdm",
        DsdNoiseShaper::Crfb => "Crfb",
    }
}

fn modulator_order(value: ModulatorOrder) -> &'static str {
    match value {
        ModulatorOrder::Order4 => "Order4",
        ModulatorOrder::Order5 => "Order5",
        ModulatorOrder::Order6 => "Order6",
        ModulatorOrder::Order7 => "Order7",
        ModulatorOrder::Order8 => "Order8",
    }
}

fn dsd_filter_preset(value: DsdFilterPreset) -> &'static str {
    match value {
        DsdFilterPreset::Auto => "Auto",
        DsdFilterPreset::Sinc => "Sinc",
    }
}

fn dsd_lowpass_method(value: DsdLowpassMethod) -> &'static str {
    match value {
        DsdLowpassMethod::Auto => "Auto",
        DsdLowpassMethod::SoxUltra => "SoxUltra",
        DsdLowpassMethod::Sinc => "Sinc",
    }
}

fn dsd_to_pcm_gain_mode(value: DsdToPcmGainMode) -> &'static str {
    match value {
        DsdToPcmGainMode::Disabled => "Disabled",
        DsdToPcmGainMode::Auto => "Auto",
        DsdToPcmGainMode::Manual => "Manual",
    }
}

fn gain_compensation(value: GainCompensation) -> String {
    match value {
        GainCompensation::Auto => "Auto".to_string(),
        GainCompensation::Linear(value) => format!("Linear({})", f32_value(value)),
        GainCompensation::Decibels(value) => format!("Decibels({})", f32_value(value)),
        GainCompensation::Disabled => "Disabled".to_string(),
    }
}

fn dsd_rate(value: crate::enums::DsdRate) -> &'static str {
    match value {
        crate::enums::DsdRate::Dsd64 => "Dsd64",
        crate::enums::DsdRate::Dsd128 => "Dsd128",
        crate::enums::DsdRate::Dsd256 => "Dsd256",
        crate::enums::DsdRate::Dsd512 => "Dsd512",
        crate::enums::DsdRate::Dsd1024 => "Dsd1024",
    }
}

fn pcm_bit_depth(value: PcmBitDepth) -> &'static str {
    match value {
        PcmBitDepth::Int8 => "Int8",
        PcmBitDepth::Int16 => "Int16",
        PcmBitDepth::Int24 => "Int24",
        PcmBitDepth::Int32 => "Int32",
        PcmBitDepth::Float32 => "Float32",
        PcmBitDepth::Float64 => "Float64",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_dsd(
        gain_mode: DsdToPcmGainMode,
        margin_db: f32,
        gain_db: Option<f32>,
    ) -> crate::settings::DsdSettings {
        let mut wire = crate::settings::LegacyDsdSettingsWireV1::default();
        wire.dsd_to_pcm_gain_mode = gain_mode;
        wire.dsd_to_pcm_auto_gain_margin_db = margin_db;
        wire.dsd_to_pcm_gain_db = gain_db;
        crate::settings::DsdSettings::from_legacy_wire(wire)
    }

    fn fingerprint_with(mut update: impl FnMut(&mut PipelineSettings)) -> SettingsFingerprint {
        let mut settings = PipelineSettings::default();
        update(&mut settings);
        settings_fingerprint(&settings)
    }

    fn test_metadata_identity(name: &str) -> ReferenceMetadataMutatorIdentityInput {
        ReferenceMetadataMutatorIdentityInput {
            canonical_path: format!("/nix/store/{name}/bin/{name}"),
            executable_sha256: Sha256Digest::of_bytes(format!("{name}-executable").as_bytes()),
            reported_version: format!("{name} 1.0"),
            closure_digest: Sha256Digest::of_bytes(format!("{name}-closure").as_bytes()),
        }
    }

    fn test_reference_execution_identity() -> ReferenceExecutionIdentityInput {
        ReferenceExecutionIdentityInput {
            planner_build_identity: "planner".to_string(),
            platform_abi_digest: Sha256Digest::of_bytes(b"platform"),
            runtime_dispatch_digest: Sha256Digest::of_bytes(b"dispatch"),
            sox_ng_sha256: Sha256Digest::of_bytes(b"sox"),
            sox_ng_version: "sox 14.8.0.1".to_string(),
            sox_ng_closure_digest: Sha256Digest::of_bytes(b"sox-closure"),
            sox_ng_behavior_probe_digest: Sha256Digest::of_bytes(b"sox-probe"),
            ffmpeg_sha256: Sha256Digest::of_bytes(b"ffmpeg"),
            ffmpeg_version: "ffmpeg 7".to_string(),
            ffmpeg_closure_digest: Sha256Digest::of_bytes(b"ffmpeg-closure"),
            ffmpeg_behavior_probe_digest: Sha256Digest::of_bytes(b"ffmpeg-probe"),
            metadata_mutators: Some(ReferenceMetadataMutatorToolchainInput {
                metaflac: test_metadata_identity("metaflac"),
                wvtag: test_metadata_identity("wvtag"),
                atomic_parsley: test_metadata_identity("AtomicParsley"),
            }),
            sacd_rs_build_identity: "sacd-rs".to_string(),
            dst_fixture_digest: Sha256Digest::of_bytes(b"dst"),
            reporting_uncertainty: DbNano(1),
            analyzer_residual: DbNano(2),
        }
    }

    #[test]
    fn execution_fingerprint_binds_every_metadata_mutator_identity_component() {
        let behavior = BehaviorFingerprintV1(Sha256Digest::of_bytes(b"behavior"));
        let semantic = SemanticPlanHashV1(Sha256Digest::of_bytes(b"semantic"));
        let qualification = Sha256Digest::of_bytes(b"qualification");
        let base = test_reference_execution_identity();
        let base_fingerprint =
            execution_fingerprint_v1(behavior, semantic, qualification, &base);

        let mut variants = Vec::new();
        for select in 0..3 {
            for component in 0..4 {
                let mut changed = base.clone();
                let mutators = changed.metadata_mutators.as_mut().expect("metadata mutators");
                let identity = match select {
                    0 => &mut mutators.metaflac,
                    1 => &mut mutators.wvtag,
                    _ => &mut mutators.atomic_parsley,
                };
                match component {
                    0 => identity.canonical_path.push_str("-changed"),
                    1 => identity.executable_sha256 = Sha256Digest::of_bytes(b"changed-executable"),
                    2 => identity.reported_version.push_str("-changed"),
                    _ => identity.closure_digest = Sha256Digest::of_bytes(b"changed-closure"),
                }
                variants.push(changed);
            }
        }
        let mut without_mutators = base.clone();
        without_mutators.metadata_mutators = None;
        variants.push(without_mutators);

        for changed in variants {
            assert_ne!(
                base_fingerprint,
                execution_fingerprint_v1(behavior, semantic, qualification, &changed),
            );
        }
    }

    #[test]
    fn disabled_dsd_to_pcm_fingerprint_ignores_auto_margin_without_legacy_gain() {
        let base = fingerprint_with(|settings| {
            settings.dsd = legacy_dsd(DsdToPcmGainMode::Disabled, 0.15, None);
        });
        let changed_stale_margin = fingerprint_with(|settings| {
            settings.dsd = legacy_dsd(DsdToPcmGainMode::Disabled, 1.0, None);
        });

        assert_eq!(base, changed_stale_margin);
    }

    #[test]
    fn disabled_dsd_to_pcm_fingerprint_honors_legacy_gain_only_when_present() {
        let no_legacy_gain = fingerprint_with(|settings| {
            settings.dsd = legacy_dsd(DsdToPcmGainMode::Disabled, 0.15, None);
        });
        let legacy_gain = fingerprint_with(|settings| {
            settings.dsd = legacy_dsd(DsdToPcmGainMode::Disabled, 0.15, Some(2.0));
        });
        let same_legacy_gain_stale_margin = fingerprint_with(|settings| {
            settings.dsd = legacy_dsd(DsdToPcmGainMode::Disabled, 1.0, Some(2.0));
        });
        let different_legacy_gain = fingerprint_with(|settings| {
            settings.dsd = legacy_dsd(DsdToPcmGainMode::Disabled, 0.15, Some(3.0));
        });

        assert_ne!(no_legacy_gain, legacy_gain);
        assert_eq!(legacy_gain, same_legacy_gain_stale_margin);
        assert_ne!(legacy_gain, different_legacy_gain);
    }

    #[test]
    fn auto_dsd_to_pcm_fingerprint_includes_margin_and_ignores_manual_gain() {
        let base = fingerprint_with(|settings| {
            settings.dsd = legacy_dsd(DsdToPcmGainMode::Auto, 0.15, None);
        });
        let stale_manual_gain = fingerprint_with(|settings| {
            settings.dsd = legacy_dsd(DsdToPcmGainMode::Auto, 0.15, Some(6.0));
        });
        let changed_margin = fingerprint_with(|settings| {
            settings.dsd = legacy_dsd(DsdToPcmGainMode::Auto, 0.50, None);
        });

        assert_eq!(base, stale_manual_gain);
        assert_ne!(base, changed_margin);
    }

    #[test]
    fn manual_dsd_to_pcm_fingerprint_includes_manual_gain_and_ignores_auto_margin() {
        let base = fingerprint_with(|settings| {
            settings.dsd = legacy_dsd(DsdToPcmGainMode::Manual, 0.15, Some(2.0));
        });
        let stale_auto_margin = fingerprint_with(|settings| {
            settings.dsd = legacy_dsd(DsdToPcmGainMode::Manual, 1.0, Some(2.0));
        });
        let changed_manual_gain = fingerprint_with(|settings| {
            settings.dsd = legacy_dsd(DsdToPcmGainMode::Manual, 0.15, Some(2.25));
        });

        assert_eq!(base, stale_auto_margin);
        assert_ne!(base, changed_manual_gain);
    }
}
