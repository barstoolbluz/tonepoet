//! Stable fingerprints for conversion settings.
//!
//! The fingerprint covers every setting that can alter conversion output. It
//! uses explicit field names and enum encodings so the digest stays independent
//! of Rust struct layout, declaration order, serde output, or debug formatting.

use sha2::{Digest, Sha256};

use crate::enums::{
    AacProfile, AudioCodec, AudioFormat, BitDepthTarget, DitherType, DsdFilterPreset, DsdLowpassMethod,
    DsdNoiseShaper, GainCompensation, ModulatorOrder, Mp3Mode,
    NyquistTransition, OpusContentType,
    PcmBitDepth, PreferredTool, RateTarget, ReplayGainMode, ResampleQuality, SoxSincPhase,
    SsrcProfile, WavPackMode,
};
use crate::dsd_reference::{
    DbNano, DsdInputFrontEnd, DsdReferencePlanSummary, DsdSourceKind, Sha256Digest,
};
use crate::plan::{PlanOperation, PlanRequest};
use crate::semantic_plan::{
    ArtifactRole, AudioState, BoundaryRepresentationContract, Claim, ClaimKind, DecisionKind,
    EffectArgumentMapping, EffectIntent, Fact, FrameExtent, GainDecisionBinding, InheritedLoudnessDisposition, LevelBasis,
    MetadataEffect, ObservationClass, ObservationKind, OutputProductIdentity, PhysicalCandidate,
    ProcessingDomain, ProgrammeState, RegisteredUnaryEffect, ReplayGainGroupBinding,
    PcmTerminalDitherOwner, PcmTerminalRealizationKind, ResolvedOperationParameters, RuntimeObligation,
    EffectPlacement, SsrcAuthorityReason, SsrcComputationPrecision, SsrcOutputRole,
    SelectedTerminalRealization, SignalCoding, StoragePrecision, TerminalProofContract, TransformContract,
    TypedConversionPlan, TypedPlanNode, ValueDomain,
};
use crate::source::{SourceInfo, SourceRepresentationKind};
use crate::settings::{
    AacSettings, DsdGeneralExportLevel, DsdGeneralReconstruction, DsdSettings, DsdToPcmSincSettings,
    FlacSettings, MetadataSettings, Mp3Settings, OpusSettings, PipelineSettings, ReplayGainSettings,
    SampleGainPolicy, SincFilterSettings, SoxResamplerSettings,
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

/// Deterministic semantic identity for the Phase-2 common planner.
///
/// This remains distinct from execution identity: executable contents, runtime
/// dispatch, ABI/environment and late-bound scalar results belong to execution
/// evidence rather than this digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CommonSemanticPlanFingerprintV1(pub Sha256Digest);

/// Deterministic Phase-2 execution-selection identity for the common plan.
///
/// Unlike [`CommonSemanticPlanFingerprintV1`], this digest intentionally binds
/// selected physical candidate identities/tools and temporary bridge ownership.
/// It is not a substitute for runtime tool/source closure attestation used by
/// qualified Reference release evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CommonExecutionPlanFingerprintV1(pub Sha256Digest);


/// Stable field-path inventory covered by [`settings_fingerprint`].
///
/// Phase 2 deliberately changes this identity domain: obsolete ambiguous DSD
/// gain fields are removed and the mutually exclusive typed gain policy is
/// fingerprinted explicitly. Runtime album scalars remain part of identity
/// whenever they can change emitted audio bytes.
pub const SETTINGS_FINGERPRINT_FIELD_PATHS: &[&str] = &[
    "target_format", "target_sample_rate", "target_bit_depth", "resample_quality",
    "nyquist_transition", "dither_type", "dither_explicit", "preferred_tool", "force_encode",
    "flac.compression_level", "flac.verify", "flac.write_md5",
    "mp3.mode", "mp3.bitrate_kbps", "mp3.vbr_quality",
    "aac.profile", "aac.bitrate_kbps", "opus.content_type", "opus.bitrate_kbps", "opus.complexity",
    "wavpack.mode", "wavpack.hybrid", "wavpack.hybrid_bitrate_kbps", "wavpack.correction_file",
    "ssrc.force", "ssrc.insane_mode", "ssrc.profile", "ssrc.attenuation_db", "ssrc.min_phase",
    "ssrc.dither_id", "ssrc.pdf_type",
    "sox_resampler.chebyshev", "sox_resampler.bandwidth_pct", "sox_resampler.phase",
    "sox_resampler.allow_aliasing", "sox_resampler.sinc_taps", "sox_resampler.sinc_attenuation_db",
    "sox_resampler.sinc_passband_hz", "sox_resampler.sinc_transition_hz",
    "sox_resampler.sinc_kaiser_beta", "sox_resampler.sinc_phase",
    "soxr_resampler.chebyshev", "soxr_resampler.cutoff", "soxr_resampler.phase",
    "dsd.pcm_to_dsd.noise_shaper", "dsd.pcm_to_dsd.modulator_order", "dsd.pcm_to_dsd.trellis",
    "dsd.pcm_to_dsd.trellis.lookahead", "dsd.pcm_to_dsd.trellis.nodes", "dsd.pcm_to_dsd.trellis.latency",
    "dsd.pcm_to_dsd.filter", "dsd.pcm_to_dsd.sinc.oversample_factor", "dsd.pcm_to_dsd.sinc.taps",
    "dsd.pcm_to_dsd.sinc.passband_hz", "dsd.pcm_to_dsd.sinc.transition_hz",
    "dsd.pcm_to_dsd.sinc.kaiser_beta", "dsd.pcm_to_dsd.sinc.linear_phase",
    "dsd.pcm_to_dsd.sinc.allow_aliasing", "dsd.pcm_to_dsd.gain_compensation",
    "dsd.from_dsd.pathway", "dsd.from_dsd.reference_policy", "dsd.from_dsd.profile",
    "dsd.from_dsd.gain.mode", "dsd.from_dsd.gain.target_dbtp", "dsd.from_dsd.gain.scope",
    "dsd.from_dsd.gain.scan", "dsd.from_dsd.gain.gain_db",
    "dsd.general_from_dsd.reconstruction", "dsd.general_from_dsd.lowpass",
    "dsd.general_from_dsd.sinc.taps", "dsd.general_from_dsd.sinc.passband_hz",
    "dsd.general_from_dsd.sinc.transition_hz", "dsd.general_from_dsd.sinc.kaiser_beta",
    "dsd.general_from_dsd.sinc.linear_phase", "dsd.general_from_dsd.sinc.allow_aliasing",
    "dsd.general_from_dsd.export_level", "dsd.general_from_dsd.export_offset_db",
    "dsd.general_from_dsd.gain.mode", "dsd.general_from_dsd.gain.target_dbtp",
    "dsd.general_from_dsd.gain.scope", "dsd.general_from_dsd.gain.scan",
    "dsd.general_from_dsd.gain.gain_db", "dsd.runtime_album_gain_db",
    "pcm_true_peak.policy.mode", "pcm_true_peak.policy.target_dbtp", "pcm_true_peak.policy.scope",
    "pcm_true_peak.policy.scan", "pcm_true_peak.policy.gain_db", "pcm_true_peak.runtime_album_gain_db",
    "metadata.transfer_tags", "metadata.preserve_artwork", "metadata.store_source_audio_md5",
    "verification.verify_after_encode", "verification.prefer_native_flac_verify",
    "replay_gain.mode", "replay_gain.prevent_clipping", "replay_gain.existing_tags",
];

/// Number of current settings identity field paths.
pub const SETTINGS_FINGERPRINT_FIELD_COUNT: usize = SETTINGS_FINGERPRINT_FIELD_PATHS.len();

/// DSD album-scoped fields that must participate in output identity.
pub const DSD_ALBUM_GAIN_FINGERPRINT_FIELD_PATHS: &[&str] = &[
    "dsd.from_dsd.gain.mode",
    "dsd.from_dsd.gain.target_dbtp",
    "dsd.from_dsd.gain.scope",
    "dsd.from_dsd.gain.scan",
    "dsd.general_from_dsd.gain.mode",
    "dsd.general_from_dsd.gain.target_dbtp",
    "dsd.general_from_dsd.gain.scope",
    "dsd.general_from_dsd.gain.scan",
    "dsd.runtime_album_gain_db",
];

/// PCM gain fields that must participate in output identity.
pub const PCM_TRUE_PEAK_FINGERPRINT_FIELD_PATHS: &[&str] = &[
    "pcm_true_peak.policy.mode",
    "pcm_true_peak.policy.target_dbtp",
    "pcm_true_peak.policy.scope",
    "pcm_true_peak.policy.scan",
    "pcm_true_peak.policy.gain_db",
    "pcm_true_peak.runtime_album_gain_db",
];

/// Strict directional DSD paths covered by the v2 settings snapshot.
pub const SETTINGS_SNAPSHOT_V2_DSD_FIELD_PATHS: &[&str] = &[
    "dsd.pcm_to_dsd.noise_shaper", "dsd.pcm_to_dsd.modulator_order", "dsd.pcm_to_dsd.trellis",
    "dsd.pcm_to_dsd.trellis.lookahead", "dsd.pcm_to_dsd.trellis.nodes", "dsd.pcm_to_dsd.trellis.latency",
    "dsd.pcm_to_dsd.filter", "dsd.pcm_to_dsd.sinc.oversample_factor", "dsd.pcm_to_dsd.sinc.taps",
    "dsd.pcm_to_dsd.sinc.passband_hz", "dsd.pcm_to_dsd.sinc.transition_hz",
    "dsd.pcm_to_dsd.sinc.kaiser_beta", "dsd.pcm_to_dsd.sinc.linear_phase",
    "dsd.pcm_to_dsd.sinc.allow_aliasing", "dsd.pcm_to_dsd.gain_compensation",
    "dsd.from_dsd.pathway", "dsd.from_dsd.reference_policy", "dsd.from_dsd.profile",
    "dsd.from_dsd.gain.mode", "dsd.from_dsd.gain.target_dbtp", "dsd.from_dsd.gain.scope",
    "dsd.from_dsd.gain.scan", "dsd.from_dsd.gain.gain_db",
    "dsd.general_from_dsd.reconstruction", "dsd.general_from_dsd.lowpass",
    "dsd.general_from_dsd.sinc.taps", "dsd.general_from_dsd.sinc.passband_hz",
    "dsd.general_from_dsd.sinc.transition_hz", "dsd.general_from_dsd.sinc.kaiser_beta",
    "dsd.general_from_dsd.sinc.linear_phase", "dsd.general_from_dsd.sinc.allow_aliasing",
    "dsd.general_from_dsd.export_level", "dsd.general_from_dsd.export_offset_db",
    "dsd.general_from_dsd.gain.mode", "dsd.general_from_dsd.gain.target_dbtp",
    "dsd.general_from_dsd.gain.scope", "dsd.general_from_dsd.gain.scan",
    "dsd.general_from_dsd.gain.gain_db", "dsd.runtime_album_gain_db",
];

/// Number of strict directional DSD fields in the settings snapshot.
pub const SETTINGS_SNAPSHOT_V2_DSD_FIELD_COUNT: usize = SETTINGS_SNAPSHOT_V2_DSD_FIELD_PATHS.len();

/// Returns a deterministic content fingerprint for all conversion-affecting
/// fields in [`PipelineSettings`].
#[must_use]
pub fn settings_fingerprint(settings: &PipelineSettings) -> SettingsFingerprint {
    let mut writer = FingerprintWriter::new();
    writer.field_static("schema", "tonepoet-pipeline-settings-fingerprint/v2");
    push_pipeline_settings(&mut writer, settings);
    SettingsFingerprint(writer.finish())
}

/// Deterministic ordinary-execution identity for conversion settings plus the
/// registered sample-domain effect chain. Empty effect chains deliberately
/// preserve the historical settings fingerprint so existing manifests remain
/// valid for unchanged requests.
#[must_use]
pub fn settings_and_effects_fingerprint(
    settings: &PipelineSettings,
    effects: &[EffectIntent],
) -> SettingsFingerprint {
    let settings_only = settings_fingerprint(settings);
    if effects.is_empty() {
        return settings_only;
    }

    let mut writer = FingerprintWriter::new();
    writer.field_static(
        "schema",
        "tonepoet-pipeline-settings-effects-fingerprint/v1",
    );
    writer.field_string("settings", settings_only.to_hex());
    for (index, effect) in effects.iter().enumerate() {
        let prefix = format!("effect.{index}");
        writer.field_string(&format!("{prefix}.id"), effect.id.0.to_string());
        for (dependency_index, dependency) in effect.after.iter().enumerate() {
            writer.field_string(
                &format!("{prefix}.after.{dependency_index}"),
                dependency.0.to_string(),
            );
        }
        push_registered_effect(&mut writer, &prefix, &effect.effect);
        if effect.placement == EffectPlacement::BeforePcmResample {
            writer.field_static(&format!("{prefix}.placement"), "before_pcm_resample");
        }
    }
    SettingsFingerprint(writer.finish())
}

/// Hash normalized common-plan semantics without filesystem staging paths.
///
/// The input/output path strings themselves are deliberately excluded. Exact
/// product identity is carried by the resolved product/container fields below.
#[must_use]
pub fn common_semantic_plan_fingerprint_v1(
    request: &PlanRequest,
    plan: &TypedConversionPlan,
) -> CommonSemanticPlanFingerprintV1 {
    let mut writer = FingerprintWriter::new();
    writer.field_static("schema", "tonepoet-common-semantic-plan/v1");
    writer.field_static(
        "source_probe",
        &reference_source_probe_digest_v1(&request.source).to_hex(),
    );
    writer.field_static("intent.target_format", &audio_format(&plan.intent.target_format));
    writer.field_string("intent.target_rate", rate_target(plan.intent.target_rate));
    writer.field_string("intent.target_depth", bit_depth_target(plan.intent.target_depth));
    push_sample_gain_policy(&mut writer, "intent.gain", plan.intent.gain_policy);
    writer.field_static(
        "intent.reference_delivery",
        bool_value(plan.intent.reference_delivery),
    );
    writer.field_static(
        "intent.dsd_reconstruction",
        match plan.intent.dsd_reconstruction {
            None => "none",
            Some(DsdGeneralReconstruction::General) => "general",
            Some(DsdGeneralReconstruction::ReferenceProtected) => "reference_protected",
        },
    );
    match plan.intent.dsd_export_level {
        Some(level) => push_dsd_export_level(&mut writer, "intent.dsd_export_level", level),
        None => writer.field_static("intent.dsd_export_level", "none"),
    }
    match plan.intent.replay_gain {
        Some(policy) => {
            writer.field_static("intent.replay_gain.mode", replay_gain_mode(policy.mode));
            writer.field_string(
                "intent.replay_gain.prevention_ceiling_dbtp",
                policy
                    .prevention_ceiling_dbtp
                    .map_or_else(|| "none".to_owned(), |ceiling| ceiling.render(false)),
            );
        }
        None => {
            writer.field_static("intent.replay_gain.mode", "none");
            writer.field_static("intent.replay_gain.prevention_ceiling_dbtp", "none");
        }
    }
    writer.field_static(
        "intent.metadata.transfer_tags",
        bool_value(plan.intent.metadata.transfer_tags),
    );
    writer.field_static(
        "intent.metadata.preserve_artwork",
        bool_value(plan.intent.metadata.preserve_artwork),
    );
    writer.field_static(
        "intent.store_source_audio_md5",
        bool_value(plan.intent.store_source_audio_md5),
    );
    writer.field_static(
        "intent.verify_after_encode",
        bool_value(plan.intent.verify_after_encode),
    );
    for (index, effect) in plan.intent.processing.iter().enumerate() {
        let prefix = format!("effect.{index}");
        writer.field_string(&format!("{prefix}.id"), effect.id.0.to_string());
        for (dependency_index, dependency) in effect.after.iter().enumerate() {
            writer.field_string(
                &format!("{prefix}.after.{dependency_index}"),
                dependency.0.to_string(),
            );
        }
        push_registered_effect(&mut writer, &prefix, &effect.effect);
    }
    for (index, state) in plan.audio_states.iter().enumerate() {
        push_audio_state(&mut writer, index, state);
    }
    for (index, node) in plan.nodes.iter().enumerate() {
        push_semantic_node(&mut writer, index, node);
    }
    for (index, artifact) in plan.artifacts.iter().enumerate() {
        let prefix = format!("artifact.{index}");
        writer.field_string(&format!("{prefix}.id"), artifact.id.0.to_string());
        writer.field_static(&format!("{prefix}.role"), artifact_role(artifact.role));
        writer.field_string(
            &format!("{prefix}.signal"),
            artifact
                .signal
                .map_or_else(|| "none".to_owned(), |signal| signal.0.to_string()),
        );
        if let Some(product) = &artifact.product {
            push_output_product(&mut writer, &format!("{prefix}.product"), product);
        }
        push_inherited_loudness(
            &mut writer,
            &format!("{prefix}.inherited_loudness"),
            &artifact.inherited_loudness,
        );
        for (effect_index, effect) in artifact.metadata_effects.iter().enumerate() {
            push_metadata_effect(
                &mut writer,
                &format!("{prefix}.metadata_effect.{effect_index}"),
                *effect,
            );
        }
        for (obligation_index, obligation) in artifact.obligations.iter().enumerate() {
            push_runtime_obligation(
                &mut writer,
                &format!("{prefix}.obligation.{obligation_index}"),
                obligation,
            );
        }
    }
    CommonSemanticPlanFingerprintV1(Sha256Digest(writer.finish()))
}

/// Hash the Phase-2 physical selection and bridge identity for a typed plan.
///
/// Filesystem staging paths are deliberately absent. The semantic digest binds
/// the normalized request/contracts; this layer adds only execution-selection
/// facts that may differ while preserving those semantics.
#[must_use]
pub fn common_execution_plan_fingerprint_v1(
    request: &PlanRequest,
    plan: &TypedConversionPlan,
) -> CommonExecutionPlanFingerprintV1 {
    let mut writer = FingerprintWriter::new();
    writer.field_static("schema", "tonepoet-common-execution-plan/v1");
    writer.field_static(
        "semantic",
        &common_semantic_plan_fingerprint_v1(request, plan).0.to_hex(),
    );
    writer.field_static(
        "execution_capability",
        match plan.execution_capability {
            crate::semantic_plan::ExecutionCapability::ExecutableNow => "executable_now",
            crate::semantic_plan::ExecutionCapability::ExecutableByPhase3CommonRealizer => {
                "executable_by_phase3_common_realizer"
            }
            crate::semantic_plan::ExecutionCapability::RequiresPhase3DsdTrackTruePeak => {
                "requires_phase3_dsd_track_true_peak"
            }
            crate::semantic_plan::ExecutionCapability::RequiresPhase3CommonRealizer => {
                "requires_phase3_common_realizer"
            }
        },
    );

    for (index, bridge) in plan.bridges.iter().enumerate() {
        writer.field_static(
            &format!("bridge.{index}"),
            match bridge {
                crate::semantic_plan::ExecutionBridge::ExistingCommandPlan => {
                    "existing_command_plan"
                }
            },
        );
    }

    for (node_index, node) in plan.nodes.iter().enumerate() {
        let TypedPlanNode::Operation {
            candidates,
            selected_candidate,
            resolved_parameters,
            ..
        } = node
        else {
            continue;
        };
        let prefix = format!("selected.{node_index}");
        writer.field_string(
            &format!("{prefix}.index"),
            selected_candidate.to_string(),
        );
        match candidates.get(*selected_candidate) {
            Some(candidate) => {
                writer.field_string(&format!("{prefix}.identity"), candidate.identity.clone());
                writer.field_string(
                    &format!("{prefix}.tool"),
                    candidate
                        .tool
                        .as_ref()
                        .map_or_else(|| "internal".to_owned(), |tool| tool.program().to_owned()),
                );
                writer.field_static(
                    &format!("{prefix}.executable"),
                    bool_value(candidate.executable),
                );
                match &candidate.contract.terminal_realization {
                    Some(realization) => push_terminal_realization(
                        &mut writer,
                        &format!("{prefix}.terminal_realization"),
                        realization,
                    ),
                    None => writer.field_static(
                        &format!("{prefix}.terminal_realization"),
                        "none",
                    ),
                }
                if matches!(
                    resolved_parameters,
                    ResolvedOperationParameters::ResampleSsrc {
                        emitted_processing_domain: ProcessingDomain::Binary64,
                        ..
                    }
                ) {
                    match candidate.binary64_resample_preservation_evidence.as_ref() {
                        Some(crate::ssrc_binary64::Binary64ResamplePreservationEvidence::Established {
                            authority_id,
                            evidence_id,
                            qualification_report_sha256,
                            runtime_attestation,
                            scope,
                        }) => {
                            writer.field_static(
                                &format!("{prefix}.binary64.contract"),
                                crate::ssrc_binary64::TONEPOET_BINARY64_OVERLOAD_PRESERVING_RESAMPLE_V1,
                            );
                            writer.field_string(
                                &format!("{prefix}.binary64.authority"),
                                authority_id.clone(),
                            );
                            writer.field_string(
                                &format!("{prefix}.binary64.evidence"),
                                evidence_id.clone(),
                            );
                            writer.field_string(
                                &format!("{prefix}.binary64.report_sha256"),
                                qualification_report_sha256.clone(),
                            );
                            writer.field_string(
                                &format!("{prefix}.binary64.executable_sha256"),
                                runtime_attestation.expected_executable_sha256.clone(),
                            );
                            writer.field_string(
                                &format!("{prefix}.binary64.architecture"),
                                runtime_attestation.architecture.clone(),
                            );
                            writer.field_string(
                                &format!("{prefix}.binary64.source_revision"),
                                runtime_attestation.source_revision.clone(),
                            );
                            writer.field_string(
                                &format!("{prefix}.binary64.build_identity"),
                                runtime_attestation.build_identity.clone(),
                            );
                            writer.field_static(
                                &format!("{prefix}.binary64.profile"),
                                ssrc_profile(scope.profile),
                            );
                            writer.field_string(
                                &format!("{prefix}.binary64.source_rate_hz"),
                                scope.source_rate_hz.to_string(),
                            );
                            writer.field_string(
                                &format!("{prefix}.binary64.target_rate_hz"),
                                scope.target_rate_hz.to_string(),
                            );
                            writer.field_string(
                                &format!("{prefix}.binary64.attenuation_db"),
                                scope.attenuation_db.clone().unwrap_or_else(|| "none".to_owned()),
                            );
                            writer.field_static(
                                &format!("{prefix}.binary64.min_phase"),
                                bool_value(scope.min_phase),
                            );
                            writer.field_string(
                                &format!("{prefix}.binary64.input_container"),
                                scope.input_container.clone(),
                            );
                            writer.field_string(
                                &format!("{prefix}.binary64.input_sample_format"),
                                scope.input_sample_format.clone(),
                            );
                            writer.field_string(
                                &format!("{prefix}.binary64.output_container"),
                                scope.output_container.clone(),
                            );
                            writer.field_string(
                                &format!("{prefix}.binary64.output_sample_format"),
                                scope.output_sample_format.clone(),
                            );
                            writer.field_static(
                                &format!("{prefix}.binary64.payload_bridge"),
                                crate::ssrc_binary64::SSRC_W64_EXACT_PAYLOAD_BRIDGE_V1,
                            );
                            match candidate.protected_float64_ingress_authority.as_ref() {
                                Some(ingress) => {
                                    writer.field_string(
                                        &format!("{prefix}.binary64.protected_ingress.authority"),
                                        ingress.authority_id.clone(),
                                    );
                                    writer.field_string(
                                        &format!("{prefix}.binary64.protected_ingress.container"),
                                        ingress.input_container.clone(),
                                    );
                                    writer.field_string(
                                        &format!("{prefix}.binary64.protected_ingress.sample_format"),
                                        ingress.input_sample_format.clone(),
                                    );
                                    writer.field_string(
                                        &format!("{prefix}.binary64.protected_ingress.max_physical_bytes"),
                                        ingress.max_physical_bytes.to_string(),
                                    );
                                    writer.field_string(
                                        &format!("{prefix}.binary64.protected_ingress.muxer_structure_upper_bound_bytes"),
                                        ingress.muxer_structure_upper_bound_bytes.to_string(),
                                    );
                                }
                                None => writer.field_static(
                                    &format!("{prefix}.binary64.protected_ingress"),
                                    "missing",
                                ),
                            }
                        }
                        _ => writer.field_static(
                            &format!("{prefix}.binary64.established_binding"),
                            "missing",
                        ),
                    }
                }
            }
            None => writer.field_static(&format!("{prefix}.invalid"), "true"),
        }
    }

    for (index, operation) in plan.lowering_bridge_operations.iter().enumerate() {
        push_plan_operation(
            &mut writer,
            &format!("lowering_bridge_operation.{index}"),
            operation,
        );
    }

    CommonExecutionPlanFingerprintV1(Sha256Digest(writer.finish()))
}

fn push_semantic_node(writer: &mut FingerprintWriter, index: usize, node: &TypedPlanNode) {
    let prefix = format!("node.{index}");
    match node {
        TypedPlanNode::Operation {
            operation,
            input_signal,
            output_signal,
            candidates,
            selected_candidate: _,
            resolved_parameters,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "operation");
            writer.field_string(
                &format!("{prefix}.input_signal"),
                input_signal.map_or_else(|| "none".to_owned(), |signal| signal.0.to_string()),
            );
            writer.field_string(
                &format!("{prefix}.output_signal"),
                output_signal.map_or_else(|| "none".to_owned(), |signal| signal.0.to_string()),
            );
            push_plan_operation(writer, &format!("{prefix}.operation"), operation);
            push_resolved_operation_parameters(
                writer,
                &format!("{prefix}.resolved"),
                resolved_parameters,
            );
            for (candidate_index, candidate) in candidates.iter().enumerate() {
                push_physical_candidate(
                    writer,
                    &format!("{prefix}.candidate.{candidate_index}"),
                    candidate,
                );
            }
        }
        TypedPlanNode::Observe(observation) => {
            writer.field_static(&format!("{prefix}.kind"), "observe");
            writer.field_string(&format!("{prefix}.id"), observation.id.0.to_string());
            writer.field_string(&format!("{prefix}.scope"), observation.scope.0.clone());
            writer.field_string(
                &format!("{prefix}.participant"),
                observation.participant.0.clone(),
            );
            writer.field_string(&format!("{prefix}.subject"), observation.subject.0.to_string());
            writer.field_string(
                &format!("{prefix}.artifact_subject"),
                observation
                    .artifact_subject
                    .map_or_else(|| "none".to_owned(), |artifact| artifact.0.to_string()),
            );
            writer.field_static(
                &format!("{prefix}.complete_reader_required"),
                bool_value(observation.complete_reader_required),
            );
            writer.field_string(
                &format!("{prefix}.read_contract.authority"),
                observation.read_contract.authority.clone(),
            );
            writer.field_static(
                &format!("{prefix}.read_contract.complete_reader"),
                bool_value(observation.read_contract.complete_reader),
            );
            writer.field_static(
                &format!("{prefix}.read_contract.connected_executor"),
                bool_value(observation.read_contract.connected_executor),
            );
            for (index, domain) in observation
                .read_contract
                .accepted_processing_domains
                .iter()
                .enumerate()
            {
                writer.field_string(
                    &format!("{prefix}.read_contract.accepted_processing.{index}"),
                    processing_domain(domain),
                );
            }
            for (index, domain) in observation
                .read_contract
                .accepted_value_domains
                .iter()
                .enumerate()
            {
                writer.field_string(
                    &format!("{prefix}.read_contract.accepted_value.{index}"),
                    value_domain(domain),
                );
            }
            match observation.kind {
                ObservationKind::CertifiedTruePeak { scan } => {
                    writer.field_static(&format!("{prefix}.metric"), "certified_true_peak");
                    writer.field_static(&format!("{prefix}.scan"), true_peak_scan(scan));
                }
                ObservationKind::ReplayGain { mode, profile, coverage } => {
                    writer.field_static(&format!("{prefix}.metric"), "replaygain");
                    writer.field_static(&format!("{prefix}.mode"), replay_gain_mode(mode));
                    writer.field_static(
                        &format!("{prefix}.profile"),
                        match profile {
                            crate::semantic_plan::ReplayGainLoudnessProfile::NativeEbu2023 => "NativeEbu2023",
                        },
                    );
                    writer.field_static(
                        &format!("{prefix}.coverage"),
                        match coverage {
                            crate::semantic_plan::ReplayGainMetricCoverage::IntegratedOnly => "integrated_only",
                            crate::semantic_plan::ReplayGainMetricCoverage::IntegratedAndRange => "integrated_and_range",
                        },
                    );
                }
                ObservationKind::SourceAudioMd5 => {
                    writer.field_static(&format!("{prefix}.metric"), "source_audio_md5");
                }
                ObservationKind::TerminalVerification => {
                    writer.field_static(&format!("{prefix}.metric"), "terminal_verification");
                }
            }
        }
        TypedPlanNode::Decide(decision) => {
            writer.field_static(&format!("{prefix}.kind"), "decide");
            writer.field_string(&format!("{prefix}.id"), decision.id.0.to_string());
            for (dependency_index, observation) in decision.observations.iter().enumerate() {
                writer.field_string(
                    &format!("{prefix}.observation.{dependency_index}.scope"),
                    observation.scope.0.clone(),
                );
                writer.field_string(
                    &format!("{prefix}.observation.{dependency_index}.participant"),
                    observation.participant.0.clone(),
                );
                writer.field_string(
                    &format!("{prefix}.observation.{dependency_index}.id"),
                    observation.observation.0.to_string(),
                );
                writer.field_static(
                    &format!("{prefix}.observation.{dependency_index}.purpose"),
                    observation_class(observation.purpose),
                );
            }
            match &decision.kind {
                DecisionKind::TruePeakGain {
                    scope,
                    target_dbtp,
                    allow_boost,
                    binding,
                    album_participant,
                } => {
                    writer.field_static(&format!("{prefix}.decision"), "true_peak_gain");
                    writer.field_static(&format!("{prefix}.scope"), true_peak_scope(*scope));
                    writer.field_string(&format!("{prefix}.target_dbtp"), target_dbtp.render(false));
                    writer.field_static(&format!("{prefix}.allow_boost"), bool_value(*allow_boost));
                    push_gain_decision_binding(writer, &format!("{prefix}.binding"), binding);
                    if let Some(participant) = album_participant {
                        writer.field_string(
                            &format!("{prefix}.album_participant.scope"),
                            participant.scope.0.clone(),
                        );
                        writer.field_string(
                            &format!("{prefix}.album_participant.participant"),
                            participant.participant.0.clone(),
                        );
                        writer.field_string(
                            &format!("{prefix}.album_participant.expected_participants"),
                            participant
                                .expected_participants
                                .map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
                        );
                        writer.field_string(
                            &format!("{prefix}.album_participant.observation.scope"),
                            participant.observation.scope.0.clone(),
                        );
                        writer.field_string(
                            &format!("{prefix}.album_participant.observation.participant"),
                            participant.observation.participant.0.clone(),
                        );
                        writer.field_string(
                            &format!("{prefix}.album_participant.observation.id"),
                            participant.observation.observation.0.to_string(),
                        );
                        writer.field_static(
                            &format!("{prefix}.album_participant.observation.purpose"),
                            observation_class(participant.observation.purpose),
                        );
                        writer.field_string(
                            &format!("{prefix}.album_participant.terminal_subject"),
                            participant.terminal_subject.0.to_string(),
                        );
                        match participant.terminal_proof.as_ref() {
                            Some(proof) => push_terminal_proof(
                                writer,
                                &format!("{prefix}.album_participant.terminal_proof"),
                                proof,
                            ),
                            None => writer.field_static(
                                &format!("{prefix}.album_participant.terminal_proof"),
                                "none",
                            ),
                        }
                    } else {
                        writer.field_static(
                            &format!("{prefix}.album_participant"),
                            "none",
                        );
                    }
                }
                DecisionKind::ReferenceGain { policy } => {
                    writer.field_static(&format!("{prefix}.decision"), "reference_gain");
                    writer.field_string(
                        &format!("{prefix}.policy"),
                        canonical_gain_policy(*policy),
                    );
                }
                DecisionKind::ReplayGainProjection { policy, group } => {
                    writer.field_static(&format!("{prefix}.decision"), "replaygain_projection");
                    writer.field_static(&format!("{prefix}.mode"), replay_gain_mode(policy.mode));
                    writer.field_string(
                        &format!("{prefix}.prevention_ceiling_dbtp"),
                        policy
                            .prevention_ceiling_dbtp
                            .map_or_else(|| "none".to_owned(), |ceiling| ceiling.render(false)),
                    );
                    push_replaygain_group_binding(writer, &format!("{prefix}.group"), group);
                }
            }
        }
        TypedPlanNode::ApplyGain {
            input,
            output,
            policy,
            decision,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "apply_gain");
            writer.field_string(&format!("{prefix}.input"), input.0.to_string());
            writer.field_string(&format!("{prefix}.output"), output.0.to_string());
            writer.field_string(
                &format!("{prefix}.decision"),
                decision.map_or_else(|| "none".to_owned(), |id| id.0.to_string()),
            );
            push_sample_gain_policy(writer, &format!("{prefix}.gain"), *policy);
        }
        TypedPlanNode::DecodeSourceForProcessing { input, output } => {
            writer.field_static(&format!("{prefix}.kind"), "decode_source_for_processing");
            writer.field_string(&format!("{prefix}.input"), input.0.to_string());
            writer.field_string(&format!("{prefix}.output"), output.0.to_string());
        }
        TypedPlanNode::ApplyEffect {
            input,
            output,
            instance,
            lowering,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "apply_effect");
            writer.field_string(&format!("{prefix}.input"), input.0.to_string());
            writer.field_string(&format!("{prefix}.output"), output.0.to_string());
            writer.field_string(&format!("{prefix}.instance"), instance.id.0.to_string());
            push_registered_effect(writer, &format!("{prefix}.effect"), &instance.effect);
            writer.field_static(&format!("{prefix}.tool"), lowering.tool.program());
            match &lowering.arguments {
                EffectArgumentMapping::SoxEffect(args) => {
                    writer.field_static(&format!("{prefix}.argument_kind"), "sox_effect");
                    for (arg_index, arg) in args.iter().enumerate() {
                        writer.field_string(&format!("{prefix}.arg.{arg_index}"), arg.clone());
                    }
                }
                EffectArgumentMapping::FfmpegAudioFilter(filter) => {
                    writer.field_static(&format!("{prefix}.argument_kind"), "ffmpeg_filter");
                    writer.field_string(&format!("{prefix}.filter"), filter.clone());
                }
            }
        }
        TypedPlanNode::ExportDsdLevel {
            input,
            output,
            level,
            gain_db,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "export_dsd_level");
            writer.field_string(&format!("{prefix}.input"), input.0.to_string());
            writer.field_string(&format!("{prefix}.output"), output.0.to_string());
            push_dsd_export_level(writer, &format!("{prefix}.level"), *level);
            writer.field_string(&format!("{prefix}.gain_db"), gain_db.render(false));
        }
        TypedPlanNode::ReferenceTerminalRealization {
            input,
            output,
            decision,
            sample_contract,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "reference_terminal_realization");
            writer.field_string(&format!("{prefix}.input"), input.0.to_string());
            writer.field_string(&format!("{prefix}.output"), output.0.to_string());
            writer.field_string(&format!("{prefix}.decision"), decision.0.to_string());
            writer.field_string(
                &format!("{prefix}.sample_rate_hz"),
                sample_contract.sample_rate_hz.to_string(),
            );
            writer.field_string(
                &format!("{prefix}.channels"),
                sample_contract.channels.to_string(),
            );
            writer.field_static(
                &format!("{prefix}.sample_kind"),
                match sample_contract.sample_kind {
                    crate::enums::SampleKind::SignedInteger => "signed_integer",
                    crate::enums::SampleKind::UnsignedInteger => "unsigned_integer",
                    crate::enums::SampleKind::Float => "float",
                    crate::enums::SampleKind::Dsd => "dsd",
                },
            );
            writer.field_static(
                &format!("{prefix}.bit_depth"),
                pcm_bit_depth(sample_contract.bit_depth),
            );
            writer.field_static(
                &format!("{prefix}.dither"),
                match sample_contract.dither {
                    crate::dsd_reference::ReferenceDither::None => "none",
                    crate::dsd_reference::ReferenceDither::Tpdf => "tpdf",
                    crate::dsd_reference::ReferenceDither::Shibata => "shibata",
                },
            );
        }
        TypedPlanNode::PackageOutput {
            input,
            output,
            product,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "package_output");
            writer.field_string(&format!("{prefix}.input"), input.0.to_string());
            writer.field_string(&format!("{prefix}.output"), output.0.to_string());
            push_output_product(writer, &format!("{prefix}.product"), product);
        }
        TypedPlanNode::MutateArtifact { input, output, effect } => {
            writer.field_static(&format!("{prefix}.kind"), "mutate_artifact");
            writer.field_string(&format!("{prefix}.input"), input.0.to_string());
            writer.field_string(&format!("{prefix}.output"), output.0.to_string());
            push_metadata_effect(writer, &format!("{prefix}.effect"), *effect);
        }
        TypedPlanNode::DecodeArtifactForObservation { input, output } => {
            writer.field_static(&format!("{prefix}.kind"), "decode_artifact_for_observation");
            writer.field_string(&format!("{prefix}.input"), input.0.to_string());
            writer.field_string(&format!("{prefix}.output"), output.0.to_string());
        }
        TypedPlanNode::VerifyArtifact { artifact, observation } => {
            writer.field_static(&format!("{prefix}.kind"), "verify_artifact");
            writer.field_string(&format!("{prefix}.artifact"), artifact.0.to_string());
            writer.field_string(&format!("{prefix}.observation"), observation.0.to_string());
        }
    }
}

fn push_audio_state(writer: &mut FingerprintWriter, index: usize, state: &AudioState) {
    let prefix = format!("state.{index}");
    writer.field_string(&format!("{prefix}.id"), state.id.0.to_string());
    writer.field_string(&format!("{prefix}.coding"), signal_coding(&state.coding));
    push_fact_u32(writer, &format!("{prefix}.sample_rate_hz"), &state.sample_rate_hz);
    push_fact_u16(writer, &format!("{prefix}.channels"), &state.channels);
    push_fact_string_vec(writer, &format!("{prefix}.channel_layout"), &state.channel_layout);
    push_frame_extent(writer, &format!("{prefix}.frame_extent"), &state.frame_extent);
    writer.field_static(&format!("{prefix}.programme"), programme_state(&state.programme));
    writer.field_string(&format!("{prefix}.precision"), storage_precision(&state.precision));
    push_fact_string(writer, &format!("{prefix}.storage_contract"), &state.storage_contract);
    push_fact_processing_domain(
        writer,
        &format!("{prefix}.processing_domain"),
        &state.processing_domain,
    );
    writer.field_string(&format!("{prefix}.value_domain"), value_domain(&state.value_domain));
    writer.field_string(&format!("{prefix}.level_basis"), level_basis(state.level_basis));
    for (claim_index, claim) in state.claims.iter().enumerate() {
        push_claim(writer, &format!("{prefix}.claim.{claim_index}"), claim);
    }
    for (obligation_index, obligation) in state.obligations.iter().enumerate() {
        push_runtime_obligation(
            writer,
            &format!("{prefix}.obligation.{obligation_index}"),
            obligation,
        );
    }
}

fn push_physical_candidate(
    writer: &mut FingerprintWriter,
    prefix: &str,
    candidate: &PhysicalCandidate,
) {
    // Implementation identity, selected tool and whether a lowerer happens to
    // be connected in this build belong to execution identity.  The common
    // semantic digest records only the proof/representation contract that is
    // applicable to this semantic route.
    push_transform_contract(writer, &format!("{prefix}.contract"), &candidate.contract);
}

fn push_transform_contract(
    writer: &mut FingerprintWriter,
    prefix: &str,
    contract: &TransformContract,
) {
    push_boundary_representation(
        writer,
        &format!("{prefix}.representation"),
        &contract.representation,
    );
    match &contract.terminal_proof {
        Some(proof) => push_terminal_proof(writer, &format!("{prefix}.terminal_proof"), proof),
        None => writer.field_static(&format!("{prefix}.terminal_proof"), "none"),
    }
    for (index, claim) in contract.carries_claims.iter().enumerate() {
        writer.field_static(
            &format!("{prefix}.carries_claim.{index}"),
            claim_kind(*claim),
        );
    }
    for (index, claim) in contract.produces_claims.iter().enumerate() {
        writer.field_static(
            &format!("{prefix}.produces_claim.{index}"),
            claim_kind(*claim),
        );
    }
    for (index, observation) in contract.signal_equivalent_for.iter().enumerate() {
        writer.field_static(
            &format!("{prefix}.signal_equivalent_for.{index}"),
            observation_class(*observation),
        );
    }
    for (index, obligation) in contract.runtime_obligations.iter().enumerate() {
        writer.field_string(
            &format!("{prefix}.runtime_obligation.{index}"),
            obligation.clone(),
        );
    }
}

fn push_boundary_representation(
    writer: &mut FingerprintWriter,
    prefix: &str,
    contract: &BoundaryRepresentationContract,
) {
    for (index, domain) in contract.accepted_processing_domains.iter().enumerate() {
        writer.field_string(
            &format!("{prefix}.accepted_processing.{index}"),
            processing_domain(domain),
        );
    }
    for (index, domain) in contract.accepted_value_domains.iter().enumerate() {
        writer.field_string(
            &format!("{prefix}.accepted_value.{index}"),
            value_domain(domain),
        );
    }
    writer.field_string(
        &format!("{prefix}.emitted_processing"),
        contract
            .emitted_processing_domain
            .as_ref()
            .map_or_else(|| "none".to_owned(), processing_domain),
    );
    writer.field_string(
        &format!("{prefix}.emitted_precision"),
        contract
            .emitted_precision
            .as_ref()
            .map_or_else(|| "none".to_owned(), storage_precision),
    );
    writer.field_string(
        &format!("{prefix}.emitted_value"),
        contract
            .emitted_value_domain
            .as_ref()
            .map_or_else(|| "none".to_owned(), value_domain),
    );
}


fn push_terminal_realization(
    writer: &mut FingerprintWriter,
    prefix: &str,
    realization: &SelectedTerminalRealization,
) {
    match realization {
        SelectedTerminalRealization::Pcm(realization) => {
            writer.field_static(&format!("{prefix}.kind"), "pcm");
            writer.field_static(
                &format!("{prefix}.shape"),
                match realization.kind {
                    PcmTerminalRealizationKind::SsrcDirectWav => "ssrc_direct_wav",
                    PcmTerminalRealizationKind::SsrcPreterminalFfmpegPackage => {
                        "ssrc_preterminal_ffmpeg_package"
                    }
                    PcmTerminalRealizationKind::SoxDirect => "sox_direct",
                    PcmTerminalRealizationKind::FfmpegDirect => "ffmpeg_direct",
                    PcmTerminalRealizationKind::SoxPreterminalFfmpegPackage => {
                        "sox_preterminal_ffmpeg_package"
                    }
                    PcmTerminalRealizationKind::SoxPreterminalWavPackHybrid => {
                        "sox_preterminal_wavpack_hybrid"
                    }
                    PcmTerminalRealizationKind::FfmpegPreterminalWavPackHybrid => {
                        "ffmpeg_preterminal_wavpack_hybrid"
                    }
                    PcmTerminalRealizationKind::NativeWavPackHybridPackage => {
                        "native_wavpack_hybrid_package"
                    }
                },
            );
            writer.field_static(
                &format!("{prefix}.selected_tool"),
                realization.selected_tool.program(),
            );
            writer.field_string(
                &format!("{prefix}.input_precision"),
                storage_precision(&realization.input_precision),
            );
            writer.field_string(
                &format!("{prefix}.input_value_domain"),
                value_domain(&realization.input_value_domain),
            );
            writer.field_static(
                &format!("{prefix}.target_format"),
                realization.target_format.extension(),
            );
            writer.field_string(
                &format!("{prefix}.target_rate_hz"),
                realization
                    .target_rate_hz
                    .map_or_else(|| "source".to_owned(), |rate| rate.to_string()),
            );
            writer.field_static(
                &format!("{prefix}.target_bit_depth"),
                pcm_bit_depth(realization.target_bit_depth),
            );
            writer.field_static(
                &format!("{prefix}.wavpack_hybrid"),
                bool_value(realization.wavpack_hybrid),
            );
            writer.field_static(
                &format!("{prefix}.effective_dither"),
                realization.effective_dither.map(dither_type).unwrap_or("none"),
            );
            writer.field_static(
                &format!("{prefix}.dither_owner"),
                match realization.dither_owner {
                    PcmTerminalDitherOwner::None => "none",
                    PcmTerminalDitherOwner::SelectedTerminal => "selected_terminal",
                    PcmTerminalDitherOwner::SoxPreterminal => "sox_preterminal",
                    PcmTerminalDitherOwner::FfmpegPreterminal => "ffmpeg_preterminal",
                    PcmTerminalDitherOwner::SsrcResampler => "ssrc_resampler",
                },
            );
            if let Some(ssrc_dither) = realization.ssrc_dither.as_ref() {
                writer.field_string(
                    &format!("{prefix}.ssrc.dither_id"),
                    option_u8(ssrc_dither.dither_id),
                );
                writer.field_string(
                    &format!("{prefix}.ssrc.pdf_type"),
                    option_static(ssrc_dither.pdf_type.map(ssrc_pdf_type)),
                );
                writer.field_static(
                    &format!("{prefix}.ssrc.dither_origin"),
                    match ssrc_dither.origin {
                        crate::plugins::SsrcDitherOrigin::None => "none",
                        crate::plugins::SsrcDitherOrigin::GlobalExact => "global_exact",
                        crate::plugins::SsrcDitherOrigin::GlobalApproximation => {
                            "global_approximation"
                        }
                        crate::plugins::SsrcDitherOrigin::NativeOverride => "native_override",
                    },
                );
                writer.field_string(
                    &format!("{prefix}.ssrc.dither_availability"),
                    match &ssrc_dither.availability {
                        crate::plugins::SsrcDitherAvailability::Inactive => "inactive".to_owned(),
                        crate::plugins::SsrcDitherAvailability::Active => "active".to_owned(),
                        crate::plugins::SsrcDitherAvailability::UnavailableForSsrcTerminal { reason } => {
                            format!("unavailable:{reason}")
                        }
                    },
                );
            }
        }
        SelectedTerminalRealization::LossyFfmpegEncoderInput {
            target_format,
            target_rate_hz,
            apply_processing,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "lossy_ffmpeg_encoder_input");
            writer.field_static(&format!("{prefix}.target_format"), target_format.extension());
            writer.field_string(
                &format!("{prefix}.target_rate_hz"),
                target_rate_hz.map_or_else(|| "source".to_owned(), |rate| rate.to_string()),
            );
            writer.field_static(
                &format!("{prefix}.apply_processing"),
                bool_value(*apply_processing),
            );
        }
    }
}

fn push_terminal_proof(
    writer: &mut FingerprintWriter,
    prefix: &str,
    proof: &TerminalProofContract,
) {
    writer.field_string(&format!("{prefix}.authority"), proof.authority.clone());
    writer.field_static(
        &format!("{prefix}.requires_non_clipping_ingress"),
        bool_value(proof.requires_non_clipping_ingress),
    );
    for (index, domain) in proof.accepted_value_domains.iter().enumerate() {
        writer.field_string(
            &format!("{prefix}.accepted_value.{index}"),
            value_domain(domain),
        );
    }
}

fn push_fact_u32(writer: &mut FingerprintWriter, path: &str, fact: &Fact<u32>) {
    match fact {
        Fact::Known(value) => writer.field_string(path, format!("known:{value}")),
        Fact::Pending(key) => writer.field_string(path, format!("pending:{key}")),
        Fact::Unavailable(reason) => writer.field_string(path, format!("unavailable:{reason}")),
    }
}

fn push_fact_u16(writer: &mut FingerprintWriter, path: &str, fact: &Fact<u16>) {
    match fact {
        Fact::Known(value) => writer.field_string(path, format!("known:{value}")),
        Fact::Pending(key) => writer.field_string(path, format!("pending:{key}")),
        Fact::Unavailable(reason) => writer.field_string(path, format!("unavailable:{reason}")),
    }
}

fn push_fact_string(writer: &mut FingerprintWriter, path: &str, fact: &Fact<String>) {
    match fact {
        Fact::Known(value) => writer.field_string(path, format!("known:{value}")),
        Fact::Pending(key) => writer.field_string(path, format!("pending:{key}")),
        Fact::Unavailable(reason) => writer.field_string(path, format!("unavailable:{reason}")),
    }
}

fn push_fact_string_vec(writer: &mut FingerprintWriter, path: &str, fact: &Fact<Vec<String>>) {
    match fact {
        Fact::Known(values) => {
            writer.field_static(&format!("{path}.state"), "known");
            for (index, value) in values.iter().enumerate() {
                writer.field_string(&format!("{path}.{index}"), value.clone());
            }
        }
        Fact::Pending(key) => writer.field_string(path, format!("pending:{key}")),
        Fact::Unavailable(reason) => writer.field_string(path, format!("unavailable:{reason}")),
    }
}

fn push_fact_processing_domain(
    writer: &mut FingerprintWriter,
    path: &str,
    fact: &Fact<ProcessingDomain>,
) {
    match fact {
        Fact::Known(value) => writer.field_string(path, format!("known:{}", processing_domain(value))),
        Fact::Pending(key) => writer.field_string(path, format!("pending:{key}")),
        Fact::Unavailable(reason) => writer.field_string(path, format!("unavailable:{reason}")),
    }
}

fn push_frame_extent(writer: &mut FingerprintWriter, path: &str, extent: &FrameExtent) {
    match extent {
        FrameExtent::Exact(frames) => writer.field_string(path, format!("exact:{frames}")),
        FrameExtent::Bounded { upper_frames } => {
            writer.field_string(path, format!("bounded:{upper_frames}"));
        }
        FrameExtent::EstimatedDurationNanos(nanos) => {
            writer.field_string(path, format!("estimated_duration_ns:{nanos}"));
        }
        FrameExtent::Pending(key) => writer.field_string(path, format!("pending:{key}")),
        FrameExtent::Unavailable(reason) => writer.field_string(path, format!("unavailable:{reason}")),
    }
}

fn signal_coding(value: &SignalCoding) -> String {
    match value {
        SignalCoding::Pcm => "pcm".to_owned(),
        SignalCoding::Dsd => "dsd".to_owned(),
        SignalCoding::Lossy(codec) => format!("lossy:{}", audio_codec(codec)),
        SignalCoding::Unknown => "unknown".to_owned(),
    }
}

fn programme_state(value: &ProgrammeState) -> &'static str {
    match value {
        ProgrammeState::IndependentTrack => "independent_track",
        ProgrammeState::ContinuousProgramme => "continuous_programme",
    }
}

fn storage_precision(value: &StoragePrecision) -> String {
    match value {
        StoragePrecision::Pcm(depth) => format!("pcm:{}", pcm_bit_depth(*depth)),
        StoragePrecision::OneBit => "one_bit".to_owned(),
        StoragePrecision::Encoded => "encoded".to_owned(),
        StoragePrecision::Pending => "pending".to_owned(),
    }
}

fn processing_domain(value: &ProcessingDomain) -> String {
    match value {
        ProcessingDomain::Source => "source".to_owned(),
        ProcessingDomain::DsdOneBit => "dsd_one_bit".to_owned(),
        ProcessingDomain::PcmInteger(depth) => format!("pcm_integer:{}", pcm_bit_depth(*depth)),
        ProcessingDomain::PcmFloating => "pcm_floating".to_owned(),
        ProcessingDomain::Binary64 => "binary64".to_owned(),
        ProcessingDomain::Registered(name) => format!("registered:{name}"),
    }
}

fn value_domain(value: &ValueDomain) -> String {
    match value {
        ValueDomain::OneBit => "one_bit".to_owned(),
        ValueDomain::IntegerLattice(depth) => format!("integer_lattice:{}", pcm_bit_depth(*depth)),
        ValueDomain::FiniteFloating => "finite_floating".to_owned(),
        ValueDomain::Q1_31DerivedBinary64 => "q1_31_derived_binary64".to_owned(),
        ValueDomain::Encoded => "encoded".to_owned(),
        ValueDomain::Pending(key) => format!("pending:{key}"),
    }
}

fn level_basis(value: LevelBasis) -> String {
    match value {
        LevelBasis::Ordinary => "ordinary".to_owned(),
        LevelBasis::ProtectedR64 => "protected_r64".to_owned(),
        LevelBasis::DsdNative => "dsd_native".to_owned(),
        LevelBasis::DsdNominalCompensated => "dsd_nominal_compensated".to_owned(),
        LevelBasis::DsdNativeWithOffset(offset) => {
            format!("dsd_native_with_offset:{}", offset.render(false))
        }
    }
}

fn push_claim(writer: &mut FingerprintWriter, prefix: &str, claim: &Claim) {
    match claim {
        Claim::ReferenceReconstruction => writer.field_static(prefix, "reference_reconstruction"),
        Claim::ReferenceDsdDelivery => writer.field_static(prefix, "reference_dsd_delivery"),
        Claim::OverloadPreservedUntil(signal) => {
            writer.field_string(prefix, format!("overload_preserved_until:{}", signal.0));
        }
        Claim::CertifiedPcmCeiling {
            signal,
            target_dbtp,
            scan,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "certified_pcm_ceiling");
            writer.field_string(&format!("{prefix}.signal"), signal.0.to_string());
            writer.field_string(&format!("{prefix}.target_dbtp"), target_dbtp.render(false));
            writer.field_static(&format!("{prefix}.scan"), true_peak_scan(*scan));
        }
        Claim::DecodedSignalEquivalent {
            from,
            to,
            observation,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "decoded_signal_equivalent");
            writer.field_string(&format!("{prefix}.from"), from.0.to_string());
            writer.field_string(&format!("{prefix}.to"), to.0.to_string());
            writer.field_static(
                &format!("{prefix}.observation"),
                observation_class(*observation),
            );
        }
        Claim::SampleIdentity { from, to } => {
            writer.field_static(&format!("{prefix}.kind"), "sample_identity");
            writer.field_string(&format!("{prefix}.from"), from.0.to_string());
            writer.field_string(&format!("{prefix}.to"), to.0.to_string());
        }
        Claim::MetadataEffectSatisfied(effect) => {
            writer.field_static(&format!("{prefix}.kind"), "metadata_effect_satisfied");
            push_metadata_effect(writer, &format!("{prefix}.effect"), *effect);
        }
    }
}

fn push_runtime_obligation(
    writer: &mut FingerprintWriter,
    prefix: &str,
    obligation: &RuntimeObligation,
) {
    match obligation {
        RuntimeObligation::CompleteReader(signal) => {
            writer.field_string(prefix, format!("complete_reader:{}", signal.0));
        }
        RuntimeObligation::TerminalErrorBound(signal) => {
            writer.field_string(prefix, format!("terminal_error_bound:{}", signal.0));
        }
        RuntimeObligation::IndependentDecode(artifact) => {
            writer.field_string(prefix, format!("independent_decode:{}", artifact.0));
        }
        RuntimeObligation::AlbumParticipantBarrier {
            scope,
            participant,
            expected_participants,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "album_participant_barrier");
            writer.field_string(&format!("{prefix}.scope"), scope.0.clone());
            writer.field_string(&format!("{prefix}.participant"), participant.0.clone());
            writer.field_string(
                &format!("{prefix}.expected_participants"),
                expected_participants.map_or_else(|| "none".to_owned(), |value| value.to_string()),
            );
        }
        RuntimeObligation::PublicationBarrier => writer.field_static(prefix, "publication_barrier"),
    }
}

fn push_gain_decision_binding(
    writer: &mut FingerprintWriter,
    prefix: &str,
    binding: &GainDecisionBinding,
) {
    match binding {
        GainDecisionBinding::Track { scope } => {
            writer.field_static(&format!("{prefix}.kind"), "track");
            writer.field_string(&format!("{prefix}.scope"), scope.0.clone());
        }
        GainDecisionBinding::SubmittedBatch {
            scope,
            participant,
            expected_participants,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "submitted_batch");
            writer.field_string(&format!("{prefix}.scope"), scope.0.clone());
            writer.field_string(&format!("{prefix}.participant"), participant.0.clone());
            writer.field_string(
                &format!("{prefix}.expected_participants"),
                expected_participants.map_or_else(|| "none".to_owned(), |value| value.to_string()),
            );
        }
    }
}

fn push_replaygain_group_binding(
    writer: &mut FingerprintWriter,
    prefix: &str,
    binding: &ReplayGainGroupBinding,
) {
    match binding {
        ReplayGainGroupBinding::Track { scope } => {
            writer.field_static(&format!("{prefix}.kind"), "track");
            writer.field_string(&format!("{prefix}.scope"), scope.0.clone());
        }
        ReplayGainGroupBinding::SubmittedBatch {
            scope,
            participant,
            expected_participants,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "submitted_batch");
            writer.field_string(&format!("{prefix}.scope"), scope.0.clone());
            writer.field_string(&format!("{prefix}.participant"), participant.0.clone());
            writer.field_string(
                &format!("{prefix}.expected_participants"),
                expected_participants.map_or_else(|| "none".to_owned(), |value| value.to_string()),
            );
        }
    }
}

fn push_resolved_operation_parameters(
    writer: &mut FingerprintWriter,
    prefix: &str,
    parameters: &ResolvedOperationParameters,
) {
    match parameters {
        ResolvedOperationParameters::None => {
            writer.field_static(&format!("{prefix}.kind"), "none");
        }
        ResolvedOperationParameters::ResampleSsrc {
            requested,
            effective_profile,
            effective_attenuation_db,
            effective_output_depth,
            output_role,
            computation_precision,
            emitted_processing_domain,
            effective_dither,
            authority_reason,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "resample_ssrc");
            writer.field_static(
                &format!("{prefix}.effective.profile"),
                ssrc_profile(*effective_profile),
            );
            writer.field_string(
                &format!("{prefix}.effective.attenuation_db"),
                option_f32(*effective_attenuation_db),
            );
            writer.field_static(
                &format!("{prefix}.min_phase"),
                bool_value(requested.min_phase),
            );
            writer.field_static(
                &format!("{prefix}.effective.output_depth"),
                pcm_bit_depth(*effective_output_depth),
            );
            writer.field_static(
                &format!("{prefix}.effective.output_role"),
                match output_role {
                    SsrcOutputRole::Nonterminal => "nonterminal",
                    SsrcOutputRole::Terminal => "terminal",
                },
            );
            writer.field_static(
                &format!("{prefix}.effective.computation_precision"),
                match computation_precision {
                    SsrcComputationPrecision::Single => "single",
                    SsrcComputationPrecision::Double => "double",
                },
            );
            writer.field_string(
                &format!("{prefix}.effective.processing_domain"),
                processing_domain(emitted_processing_domain),
            );
            writer.field_static(
                &format!("{prefix}.authority"),
                match authority_reason {
                    SsrcAuthorityReason::ExplicitForce => "explicit_force",
                    SsrcAuthorityReason::CapabilitySelected => "capability_selected",
                },
            );
            writer.field_string(
                &format!("{prefix}.effective.dither_id"),
                option_u8(effective_dither.dither_id),
            );
            writer.field_string(
                &format!("{prefix}.effective.pdf_type"),
                option_static(effective_dither.pdf_type.map(ssrc_pdf_type)),
            );
            writer.field_static(
                &format!("{prefix}.effective.dither_origin"),
                match effective_dither.origin {
                    crate::plugins::SsrcDitherOrigin::None => "none",
                    crate::plugins::SsrcDitherOrigin::GlobalExact => "global_exact",
                    crate::plugins::SsrcDitherOrigin::GlobalApproximation => "global_approximation",
                    crate::plugins::SsrcDitherOrigin::NativeOverride => "native_override",
                },
            );
            writer.field_string(
                &format!("{prefix}.effective.dither_availability"),
                match &effective_dither.availability {
                    crate::plugins::SsrcDitherAvailability::Inactive => "inactive".to_owned(),
                    crate::plugins::SsrcDitherAvailability::Active => "active".to_owned(),
                    crate::plugins::SsrcDitherAvailability::UnavailableForSsrcTerminal { reason } => {
                        format!("unavailable:{reason}")
                    }
                },
            );
        }
        ResolvedOperationParameters::ResampleSox {
            requested,
            quality,
            effective_bandwidth_pct,
            effective_sinc_passband_hz,
            effective_dither,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "resample_sox");
            writer.field_static(
                &format!("{prefix}.rate.quality"),
                resample_quality(*quality),
            );
            writer.field_static(
                &format!("{prefix}.rate.chebyshev"),
                bool_value(requested.chebyshev),
            );
            writer.field_string(
                &format!("{prefix}.rate.bandwidth_pct"),
                option_f32(*effective_bandwidth_pct),
            );
            writer.field_string(
                &format!("{prefix}.rate.phase"),
                option_u8(requested.phase),
            );
            writer.field_static(
                &format!("{prefix}.rate.allow_aliasing"),
                bool_value(requested.allow_aliasing),
            );
            writer.field_string(
                &format!("{prefix}.effective_dither"),
                effective_dither
                    .map(dither_type)
                    .unwrap_or("none")
                    .to_owned(),
            );

            // SoX only inserts the sinc pre-filter when at least one numeric
            // sinc control is present.  A phase-only dormant setting therefore
            // must not perturb normalized semantic identity.
            let sinc_active = requested.sinc_taps.is_some()
                || requested.sinc_attenuation_db.is_some()
                || requested.sinc_passband_hz.is_some()
                || requested.sinc_transition_hz.is_some()
                || requested.sinc_kaiser_beta.is_some();
            writer.field_static(
                &format!("{prefix}.sinc.active"),
                bool_value(sinc_active),
            );
            if sinc_active {
                writer.field_string(
                    &format!("{prefix}.sinc.taps"),
                    requested
                        .sinc_taps
                        .map_or_else(|| "None".to_owned(), |value| value.to_string()),
                );
                writer.field_string(
                    &format!("{prefix}.sinc.attenuation_db"),
                    requested
                        .sinc_attenuation_db
                        .map_or_else(|| "None".to_owned(), |value| value.to_string()),
                );
                writer.field_string(
                    &format!("{prefix}.sinc.passband_hz"),
                    option_f32(*effective_sinc_passband_hz),
                );
                writer.field_string(
                    &format!("{prefix}.sinc.transition_hz"),
                    option_f32(requested.sinc_transition_hz),
                );
                writer.field_string(
                    &format!("{prefix}.sinc.kaiser_beta"),
                    option_f32(requested.sinc_kaiser_beta),
                );
                writer.field_static(
                    &format!("{prefix}.sinc.phase"),
                    match requested.sinc_phase {
                        Some(SoxSincPhase::Linear) => "linear",
                        Some(SoxSincPhase::Minimum) => "minimum",
                        Some(SoxSincPhase::Intermediate) => "intermediate",
                        None => "default_linear",
                    },
                );
            }
        }
        ResolvedOperationParameters::ResampleSoxr {
            requested,
            effective_precision,
            effective_cutoff,
            effective_phase,
            effective_dither,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "resample_soxr");
            writer.field_static(
                &format!("{prefix}.chebyshev"),
                bool_value(requested.chebyshev),
            );
            writer.field_string(
                &format!("{prefix}.effective_precision"),
                effective_precision.to_string(),
            );
            writer.field_string(
                &format!("{prefix}.effective_cutoff"),
                f32_value(*effective_cutoff),
            );
            writer.field_string(
                &format!("{prefix}.effective_phase"),
                option_u8(*effective_phase),
            );
            writer.field_string(
                &format!("{prefix}.effective_dither"),
                effective_dither
                    .map(dither_type)
                    .unwrap_or("none")
                    .to_owned(),
            );
        }
        ResolvedOperationParameters::DsdToPcm {
            requested,
            effective_sinc,
            effective_dither,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "dsd_to_pcm");
            writer.field_static(
                &format!("{prefix}.reconstruction"),
                match requested.reconstruction {
                    DsdGeneralReconstruction::General => "general",
                    DsdGeneralReconstruction::ReferenceProtected => "reference_protected",
                },
            );
            writer.field_static(
                &format!("{prefix}.lowpass"),
                dsd_lowpass_method(requested.lowpass),
            );
            if requested.lowpass == DsdLowpassMethod::Sinc {
                if let Some(sinc) = effective_sinc {
                    push_resolved_dsd_to_pcm_sinc(writer, &format!("{prefix}.sinc"), *sinc);
                }
            }
            // Export level and ordinary gain are represented by dedicated
            // semantic nodes; do not hash the same settings again here.
            writer.field_string(
                &format!("{prefix}.effective_dither"),
                effective_dither
                    .map(dither_type)
                    .unwrap_or("none")
                    .to_owned(),
            );
        }
        ResolvedOperationParameters::PcmToDsd {
            requested,
            effective_sinc,
            effective_gain_compensation,
        } => {
            push_pcm_to_dsd_resolved(
                writer,
                prefix,
                requested,
                effective_sinc.as_ref(),
                *effective_gain_compensation,
            );
        }
        ResolvedOperationParameters::DsdRateChange {
            from_dsd,
            effective_from_sinc,
            to_dsd,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "dsd_rate_change");
            writer.field_static(
                &format!("{prefix}.from.lowpass"),
                dsd_lowpass_method(from_dsd.lowpass),
            );
            if from_dsd.lowpass == DsdLowpassMethod::Sinc {
                if let Some(sinc) = effective_from_sinc {
                    push_resolved_dsd_to_pcm_sinc(writer, &format!("{prefix}.from.sinc"), *sinc);
                }
            }
            push_pcm_to_dsd_resolved(
                writer,
                &format!("{prefix}.to"),
                to_dsd,
                Some(&to_dsd.sinc),
                to_dsd.gain_compensation,
            );
        }
        ResolvedOperationParameters::EncodeFlac {
            requested,
            effective_dither,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "encode_flac");
            writer.field_string(
                &format!("{prefix}.compression_level"),
                requested.compression_level.to_string(),
            );
            // Verification is already a distinct semantic observation/barrier.
            // write_md5 changes encoded FLAC bytes where the selected backend
            // implements it, so retain the requested product policy here.
            writer.field_static(
                &format!("{prefix}.write_md5"),
                bool_value(requested.write_md5),
            );
            writer.field_string(
                &format!("{prefix}.effective_dither"),
                effective_dither
                    .map(dither_type)
                    .unwrap_or("none")
                    .to_owned(),
            );
        }
        ResolvedOperationParameters::EncodeMp3 { requested } => {
            writer.field_static(&format!("{prefix}.kind"), "encode_mp3");
            writer.field_static(&format!("{prefix}.mode"), mp3_mode(requested.mode));
            match requested.mode {
                Mp3Mode::Vbr => writer.field_string(
                    &format!("{prefix}.vbr_quality"),
                    requested.vbr_quality.to_string(),
                ),
                Mp3Mode::Cbr | Mp3Mode::Abr => writer.field_string(
                    &format!("{prefix}.bitrate_kbps"),
                    requested.bitrate_kbps.to_string(),
                ),
            }
        }
        ResolvedOperationParameters::EncodeAac { requested } => {
            writer.field_static(&format!("{prefix}.kind"), "encode_aac");
            writer.field_static(&format!("{prefix}.profile"), aac_profile(requested.profile));
            writer.field_string(
                &format!("{prefix}.bitrate_kbps"),
                requested.bitrate_kbps.to_string(),
            );
        }
        ResolvedOperationParameters::EncodeOpus { requested } => {
            writer.field_static(&format!("{prefix}.kind"), "encode_opus");
            writer.field_static(
                &format!("{prefix}.content_type"),
                opus_content_type(requested.content_type),
            );
            writer.field_string(
                &format!("{prefix}.bitrate_kbps"),
                requested.bitrate_kbps.to_string(),
            );
            writer.field_string(
                &format!("{prefix}.complexity"),
                requested.complexity.to_string(),
            );
        }
        ResolvedOperationParameters::EncodeWavPack {
            requested,
            effective_dither,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "encode_wavpack");
            writer.field_static(&format!("{prefix}.mode"), wavpack_mode(requested.mode));
            writer.field_static(
                &format!("{prefix}.hybrid"),
                bool_value(requested.hybrid),
            );
            if requested.hybrid {
                writer.field_string(
                    &format!("{prefix}.hybrid_bitrate_kbps"),
                    requested.hybrid_bitrate_kbps.to_string(),
                );
                writer.field_static(
                    &format!("{prefix}.correction_file"),
                    bool_value(requested.correction_file),
                );
            }
            writer.field_string(
                &format!("{prefix}.effective_dither"),
                effective_dither
                    .map(dither_type)
                    .unwrap_or("none")
                    .to_owned(),
            );
        }
        ResolvedOperationParameters::EncodePcm { effective_dither } => {
            writer.field_static(&format!("{prefix}.kind"), "encode_pcm");
            writer.field_string(
                &format!("{prefix}.effective_dither"),
                effective_dither
                    .map(dither_type)
                    .unwrap_or("none")
                    .to_owned(),
            );
        }
    }
}

fn push_resolved_dsd_to_pcm_sinc(
    writer: &mut FingerprintWriter,
    prefix: &str,
    sinc: crate::settings::DsdToPcmSincSettings,
) {
    writer.field_string(&format!("{prefix}.taps"), sinc.taps.to_string());
    writer.field_string(
        &format!("{prefix}.passband_hz"),
        f32_value(sinc.passband_hz),
    );
    writer.field_string(
        &format!("{prefix}.transition_hz"),
        f32_value(sinc.transition_hz),
    );
    writer.field_string(
        &format!("{prefix}.kaiser_beta"),
        f32_value(sinc.kaiser_beta),
    );
    writer.field_static(
        &format!("{prefix}.linear_phase"),
        bool_value(sinc.linear_phase),
    );
    writer.field_static(
        &format!("{prefix}.allow_aliasing"),
        bool_value(sinc.allow_aliasing),
    );
}

fn push_pcm_to_dsd_resolved(
    writer: &mut FingerprintWriter,
    prefix: &str,
    requested: &crate::settings::PcmToDsdSettings,
    effective_sinc: Option<&crate::settings::PcmToDsdSincSettings>,
    effective_gain_compensation: GainCompensation,
) {
    writer.field_static(&format!("{prefix}.kind"), "pcm_to_dsd");
    writer.field_static(
        &format!("{prefix}.noise_shaper"),
        dsd_noise_shaper(requested.noise_shaper),
    );
    writer.field_static(
        &format!("{prefix}.modulator_order"),
        modulator_order(requested.modulator_order),
    );
    writer.field_string(
        &format!("{prefix}.trellis"),
        option_trellis(requested.trellis),
    );
    if let Some(trellis) = requested.trellis {
        writer.field_string(
            &format!("{prefix}.trellis.lookahead"),
            trellis.lookahead.to_string(),
        );
        writer.field_string(
            &format!("{prefix}.trellis.nodes"),
            trellis.nodes.to_string(),
        );
        writer.field_string(
            &format!("{prefix}.trellis.latency"),
            option_u16(trellis.latency),
        );
    }
    writer.field_static(
        &format!("{prefix}.filter"),
        dsd_filter_preset(requested.filter),
    );
    if requested.filter == DsdFilterPreset::Sinc {
        if let Some(sinc) = effective_sinc {
            writer.field_string(
                &format!("{prefix}.sinc.oversample_factor"),
                sinc.oversample_factor.to_string(),
            );
            writer.field_string(
                &format!("{prefix}.sinc.taps"),
                sinc.taps.to_string(),
            );
            writer.field_string(
                &format!("{prefix}.sinc.passband_hz"),
                f32_value(sinc.passband_hz),
            );
            writer.field_string(
                &format!("{prefix}.sinc.transition_hz"),
                f32_value(sinc.transition_hz),
            );
            writer.field_string(
                &format!("{prefix}.sinc.kaiser_beta"),
                f32_value(sinc.kaiser_beta),
            );
            writer.field_static(
                &format!("{prefix}.sinc.linear_phase"),
                bool_value(sinc.linear_phase),
            );
            writer.field_static(
                &format!("{prefix}.sinc.allow_aliasing"),
                bool_value(sinc.allow_aliasing),
            );
        }
        writer.field_string(
            &format!("{prefix}.gain_compensation"),
            gain_compensation(effective_gain_compensation),
        );
    }
}

fn artifact_role(value: ArtifactRole) -> &'static str {
    match value {
        ArtifactRole::Source => "source",
        ArtifactRole::Output => "output",
    }
}

fn push_metadata_effect(writer: &mut FingerprintWriter, prefix: &str, value: MetadataEffect) {
    match value {
        MetadataEffect::TransferSource(policy) => {
            writer.field_static(&format!("{prefix}.kind"), "transfer_source");
            writer.field_static(
                &format!("{prefix}.transfer_tags"),
                bool_value(policy.transfer_tags),
            );
            writer.field_static(
                &format!("{prefix}.preserve_artwork"),
                bool_value(policy.preserve_artwork),
            );
        }
        MetadataEffect::StoreSourceAudioMd5 => {
            writer.field_static(&format!("{prefix}.kind"), "store_source_audio_md5");
        }
        MetadataEffect::ReplayGain => {
            writer.field_static(&format!("{prefix}.kind"), "replaygain");
        }
        MetadataEffect::ResolveInheritedLoudness => {
            writer.field_static(&format!("{prefix}.kind"), "resolve_inherited_loudness");
        }
    }
}

fn push_inherited_loudness(
    writer: &mut FingerprintWriter,
    prefix: &str,
    disposition: &InheritedLoudnessDisposition,
) {
    match disposition {
        InheritedLoudnessDisposition::NotCopied => writer.field_static(prefix, "not_copied"),
        InheritedLoudnessDisposition::PreserveApplicable => {
            writer.field_static(prefix, "preserve_applicable");
        }
        InheritedLoudnessDisposition::DropInapplicable => {
            writer.field_static(prefix, "drop_inapplicable");
        }
        InheritedLoudnessDisposition::Requested {
            mode,
            existing_tags,
            inherited_applicable,
        } => {
            writer.field_static(&format!("{prefix}.kind"), "requested");
            writer.field_static(&format!("{prefix}.mode"), replay_gain_mode(*mode));
            writer.field_static(
                &format!("{prefix}.existing_tags"),
                match existing_tags {
                    crate::ReplayGainExistingTagPolicy::Rescan => "rescan",
                    crate::ReplayGainExistingTagPolicy::SkipIfComplete => "skip_if_complete",
                },
            );
            writer.field_static(
                &format!("{prefix}.inherited_applicable"),
                bool_value(*inherited_applicable),
            );
        }
    }
}

fn claim_kind(value: ClaimKind) -> &'static str {
    match value {
        ClaimKind::ReferenceReconstruction => "reference_reconstruction",
        ClaimKind::ReferenceDsdDelivery => "reference_dsd_delivery",
        ClaimKind::CertifiedPcmCeiling => "certified_pcm_ceiling",
        ClaimKind::SampleIdentity => "sample_identity",
        ClaimKind::MetadataEffectSatisfied => "metadata_effect_satisfied",
    }
}

fn observation_class(value: ObservationClass) -> &'static str {
    match value {
        ObservationClass::CertifiedTruePeak => "certified_true_peak",
        ObservationClass::ReportingPeak => "reporting_peak",
        ObservationClass::Loudness => "loudness",
        ObservationClass::ProgrammeGeometry => "programme_geometry",
        ObservationClass::SourceAudioDigest => "source_audio_digest",
        ObservationClass::TerminalVerification => "terminal_verification",
    }
}

fn push_registered_effect(
    writer: &mut FingerprintWriter,
    prefix: &str,
    effect: &RegisteredUnaryEffect,
) {
    match effect {
        RegisteredUnaryEffect::SoxHighPass { frequency_hz } => {
            writer.field_static(&format!("{prefix}.kind"), "sox_highpass");
            writer.field_string(&format!("{prefix}.frequency_hz"), frequency_hz.to_string());
        }
        RegisteredUnaryEffect::SoxLowPass { frequency_hz } => {
            writer.field_static(&format!("{prefix}.kind"), "sox_lowpass");
            writer.field_string(&format!("{prefix}.frequency_hz"), frequency_hz.to_string());
        }
        RegisteredUnaryEffect::SoxSamplePeakNormalize { target_dbfs } => {
            writer.field_static(&format!("{prefix}.kind"), "sox_sample_peak_normalize");
            writer.field_string(&format!("{prefix}.target_dbfs"), target_dbfs.render(false));
        }
        RegisteredUnaryEffect::FfmpegHighPass { frequency_hz } => {
            writer.field_static(&format!("{prefix}.kind"), "ffmpeg_highpass");
            writer.field_string(&format!("{prefix}.frequency_hz"), frequency_hz.to_string());
        }
        RegisteredUnaryEffect::FfmpegLowPass { frequency_hz } => {
            writer.field_static(&format!("{prefix}.kind"), "ffmpeg_lowpass");
            writer.field_string(&format!("{prefix}.frequency_hz"), frequency_hz.to_string());
        }
    }
}

fn push_output_product(
    writer: &mut FingerprintWriter,
    prefix: &str,
    product: &OutputProductIdentity,
) {
    writer.field_string(&format!("{prefix}.format"), audio_format(&product.target_format));
    writer.field_string(
        &format!("{prefix}.extension"),
        product.container_extension.clone(),
    );
    writer.field_static(
        &format!("{prefix}.catalog_target"),
        product.catalog_target.map_or("none", |target| target.key()),
    );
    for (index, flag) in product.container_ffmpeg_flags.iter().enumerate() {
        writer.field_string(&format!("{prefix}.flag.{index}"), flag.clone());
    }
    writer.field_static(&format!("{prefix}.wavpack_hybrid"), bool_value(product.wavpack_hybrid));
    writer.field_static(
        &format!("{prefix}.wavpack_correction_file"),
        bool_value(product.wavpack_correction_file),
    );
}

fn push_dsd_export_level(writer: &mut FingerprintWriter, path: &str, level: DsdGeneralExportLevel) {
    match level {
        DsdGeneralExportLevel::Native => writer.field_static(path, "native"),
        DsdGeneralExportLevel::NominalCompensated => writer.field_static(path, "nominal_compensated"),
        DsdGeneralExportLevel::ProtectedR64 => writer.field_static(path, "protected_r64"),
        DsdGeneralExportLevel::NativeWithOffset { offset_db } => {
            writer.field_string(path, format!("native_with_offset:{}", offset_db.render(false)));
        }
    }
}

fn push_plan_operation(writer: &mut FingerprintWriter, prefix: &str, operation: &PlanOperation) {
    writer.field_static(&format!("{prefix}.kind"), operation.label());
    match operation {
        PlanOperation::DecodeToPcm { bit_depth } => {
            writer.field_static(&format!("{prefix}.bit_depth"), pcm_bit_depth(*bit_depth));
        }
        PlanOperation::ResamplePcm { target_rate_hz, target_bit_depth, profile, brick_wall } => {
            writer.field_string(&format!("{prefix}.rate"), target_rate_hz.to_string());
            writer.field_string(
                &format!("{prefix}.depth"),
                target_bit_depth.map_or_else(|| "none".to_owned(), |depth| pcm_bit_depth(depth).to_owned()),
            );
            writer.field_string(
                &format!("{prefix}.profile"),
                profile.map_or_else(|| "none".to_owned(), |value| ssrc_profile(value).to_owned()),
            );
            writer.field_static(&format!("{prefix}.brick_wall"), bool_value(*brick_wall));
        }
        PlanOperation::EncodePcm { target_format, target_rate_hz, target_bit_depth, apply_processing } => {
            writer.field_string(&format!("{prefix}.format"), audio_format(target_format));
            writer.field_string(&format!("{prefix}.rate"), target_rate_hz.map_or_else(|| "none".to_owned(), |value| value.to_string()));
            writer.field_static(&format!("{prefix}.depth"), pcm_bit_depth(*target_bit_depth));
            writer.field_static(&format!("{prefix}.processing"), bool_value(*apply_processing));
        }
        PlanOperation::EncodeLossy { target_format, target_rate_hz, apply_processing } => {
            writer.field_string(&format!("{prefix}.format"), audio_format(target_format));
            writer.field_string(&format!("{prefix}.rate"), target_rate_hz.map_or_else(|| "none".to_owned(), |value| value.to_string()));
            writer.field_static(&format!("{prefix}.processing"), bool_value(*apply_processing));
        }
        PlanOperation::PcmToDsd { target_format, target_rate, filter } => {
            writer.field_string(&format!("{prefix}.format"), audio_format(target_format));
            writer.field_string(&format!("{prefix}.rate"), target_rate.hz().to_string());
            writer.field_static(&format!("{prefix}.filter"), dsd_filter_preset(*filter));
        }
        PlanOperation::DsdToPcm { target_format, target_rate_hz, target_bit_depth, lowpass } => {
            writer.field_string(&format!("{prefix}.format"), audio_format(target_format));
            writer.field_string(&format!("{prefix}.rate"), target_rate_hz.to_string());
            writer.field_static(&format!("{prefix}.depth"), pcm_bit_depth(*target_bit_depth));
            writer.field_static(&format!("{prefix}.lowpass"), dsd_lowpass_method(*lowpass));
        }
        PlanOperation::DsdRateChange { target_format, target_rate, lowpass } => {
            writer.field_string(&format!("{prefix}.format"), audio_format(target_format));
            writer.field_string(&format!("{prefix}.rate"), target_rate.hz().to_string());
            writer.field_static(&format!("{prefix}.lowpass"), dsd_lowpass_method(*lowpass));
        }
        PlanOperation::MetadataTransfer { target_format, transfer_tags, preserve_artwork } => {
            writer.field_string(&format!("{prefix}.format"), audio_format(target_format));
            writer.field_static(&format!("{prefix}.tags"), bool_value(*transfer_tags));
            writer.field_static(&format!("{prefix}.artwork"), bool_value(*preserve_artwork));
        }
        PlanOperation::StoreSourceAudioMd5 { target_format }
        | PlanOperation::Verify { target_format } => {
            writer.field_string(&format!("{prefix}.format"), audio_format(target_format));
        }
    }
}

/// Historical manifest field type. The manifest slot name remains stable while
/// Phase 2 deliberately changes the settings identity domain to the strict schema.
pub type LegacySettingsFingerprintV1 = SettingsFingerprint;

/// Return the current strict settings identity for the historical manifest slot.
#[must_use]
pub fn legacy_settings_fingerprint_v1(
    settings: &PipelineSettings,
) -> LegacySettingsFingerprintV1 {
    settings_fingerprint(settings)
}

/// Canonical settings-snapshot v2 digest.
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

/// Exact tool/runtime closure inputs for qualified Reference execution identity.
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
    /// Content-bound fingerprint of the plan-independent common planner/observer/
    /// reader/terminal/package/build closure characterized by Phase-5 qualification.
    /// Optional metadata-mutator identities are bound separately above.
    pub common_runtime_closure_fingerprint_sha256: String,
}

/// Hash the canonical settings-snapshot v2 representation. This is audit identity, not
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
        "common_runtime_closure_fingerprint_sha256",
        &identity.common_runtime_closure_fingerprint_sha256,
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
    if settings.dither_explicit {
        writer.field_static("dither_explicit", "true");
    }
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
    push_pcm_true_peak(writer, &settings.pcm_true_peak);
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
    if settings.dither_explicit {
        writer.field_static("dither_explicit", "true");
    }
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
    push_pcm_true_peak(writer, &settings.pcm_true_peak);
    push_metadata(writer, &settings.metadata);
    push_verification(writer, &settings.verification);
    push_replay_gain(writer, &settings.replay_gain);
}

fn push_pcm_true_peak(writer: &mut FingerprintWriter, settings: &crate::PcmTruePeakGainSettings) {
    push_sample_gain_policy(writer, "pcm_true_peak.policy", settings.policy);
    writer.field_string(
        "pcm_true_peak.runtime_album_gain_db",
        option_db_nano(settings.runtime_album_gain_db()),
    );
}

fn push_sample_gain_policy(writer: &mut FingerprintWriter, prefix: &str, policy: SampleGainPolicy) {
    let mode_path = format!("{prefix}.mode");
    let target_path = format!("{prefix}.target_dbtp");
    let scope_path = format!("{prefix}.scope");
    let scan_path = format!("{prefix}.scan");
    let gain_path = format!("{prefix}.gain_db");
    match policy {
        SampleGainPolicy::Off => {
            writer.field_static(&mode_path, "off");
            writer.field_static(&target_path, "None");
            writer.field_static(&scope_path, "None");
            writer.field_static(&scan_path, "None");
            writer.field_static(&gain_path, "None");
        }
        SampleGainPolicy::TruePeakGuard { target_dbtp, scope, scan } => {
            writer.field_static(&mode_path, "true_peak_guard");
            writer.field_string(&target_path, target_dbtp.render(false));
            writer.field_static(&scope_path, true_peak_scope(scope));
            writer.field_static(&scan_path, true_peak_scan(scan));
            writer.field_static(&gain_path, "None");
        }
        SampleGainPolicy::TruePeakNormalize { target_dbtp, scope, scan } => {
            writer.field_static(&mode_path, "true_peak_normalize");
            writer.field_string(&target_path, target_dbtp.render(false));
            writer.field_static(&scope_path, true_peak_scope(scope));
            writer.field_static(&scan_path, true_peak_scan(scan));
            writer.field_static(&gain_path, "None");
        }
        SampleGainPolicy::FixedGain { gain_db } => {
            writer.field_static(&mode_path, "fixed_gain");
            writer.field_static(&target_path, "None");
            writer.field_static(&scope_path, "None");
            writer.field_static(&scan_path, "None");
            writer.field_string(&gain_path, gain_db.render(false));
        }
    }
}

fn push_dsd_to_pcm_sinc(writer: &mut FingerprintWriter, settings: &DsdToPcmSincSettings) {
    writer.field_string("dsd.general_from_dsd.sinc.taps", settings.taps.to_string());
    writer.field_string("dsd.general_from_dsd.sinc.passband_hz", f32_value(settings.passband_hz));
    writer.field_string("dsd.general_from_dsd.sinc.transition_hz", f32_value(settings.transition_hz));
    writer.field_string("dsd.general_from_dsd.sinc.kaiser_beta", f32_value(settings.kaiser_beta));
    writer.field_static("dsd.general_from_dsd.sinc.linear_phase", bool_value(settings.linear_phase));
    writer.field_static("dsd.general_from_dsd.sinc.allow_aliasing", bool_value(settings.allow_aliasing));
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
        crate::ResolvedGainPolicy::TruePeakNormalize {
            target_dbtp,
            scope,
            scan,
            bound_gain,
            terminal_bound,
        } => format!(
            "auto:{}:{scope:?}:{scan:?}:{}:{}:{}",
            target_dbtp.render(false),
            option_db_nano(bound_gain),
            terminal_bound.max_added_peak_fs_q63_ceil,
            terminal_bound.derivation_digest.to_hex(),
        ),
        crate::ResolvedGainPolicy::Off { ceiling, terminal_bound } => format!(
            "off:{}:{}:{}",
            ceiling.render(false),
            terminal_bound.max_added_peak_fs_q63_ceil,
            terminal_bound.derivation_digest.to_hex(),
        ),
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
    writer.field_static("dsd.pcm_to_dsd.noise_shaper", dsd_noise_shaper(pcm.noise_shaper));
    writer.field_static("dsd.pcm_to_dsd.modulator_order", modulator_order(pcm.modulator_order));
    writer.field_string("dsd.pcm_to_dsd.trellis", option_trellis(pcm.trellis));
    if let Some(trellis) = pcm.trellis {
        writer.field_string("dsd.pcm_to_dsd.trellis.lookahead", trellis.lookahead.to_string());
        writer.field_string("dsd.pcm_to_dsd.trellis.nodes", trellis.nodes.to_string());
        writer.field_string("dsd.pcm_to_dsd.trellis.latency", option_u16(trellis.latency));
    } else {
        writer.field_static("dsd.pcm_to_dsd.trellis.lookahead", "None");
        writer.field_static("dsd.pcm_to_dsd.trellis.nodes", "None");
        writer.field_static("dsd.pcm_to_dsd.trellis.latency", "None");
    }
    writer.field_static("dsd.pcm_to_dsd.filter", dsd_filter_preset(pcm.filter));
    push_sinc(writer, &pcm.sinc);
    writer.field_string("dsd.pcm_to_dsd.gain_compensation", gain_compensation(pcm.gain_compensation));

    let from = settings.from_dsd;
    writer.field_static("dsd.from_dsd.pathway", match from.pathway {
        crate::DsdSourcePathway::Custom => "custom",
        crate::DsdSourcePathway::Reference => "reference",
        crate::DsdSourcePathway::Manual => "manual",
    });
    writer.field_static("dsd.from_dsd.reference_policy", from.reference_policy.key());
    writer.field_static("dsd.from_dsd.profile", match from.profile {
        crate::DsdReconstructionSelection::Reference => "reference",
        crate::DsdReconstructionSelection::Wideband => "wideband",
    });
    push_sample_gain_policy(writer, "dsd.from_dsd.gain", from.gain);

    let general = settings.general_from_dsd;
    writer.field_static("dsd.general_from_dsd.reconstruction", match general.reconstruction {
        DsdGeneralReconstruction::General => "general",
        DsdGeneralReconstruction::ReferenceProtected => "reference_protected",
    });
    writer.field_static("dsd.general_from_dsd.lowpass", dsd_lowpass_method(general.lowpass));
    push_dsd_to_pcm_sinc(writer, &general.sinc);
    match general.export_level {
        DsdGeneralExportLevel::Native => {
            writer.field_static("dsd.general_from_dsd.export_level", "native");
            writer.field_static("dsd.general_from_dsd.export_offset_db", "None");
        }
        DsdGeneralExportLevel::NominalCompensated => {
            writer.field_static("dsd.general_from_dsd.export_level", "nominal_compensated");
            writer.field_static("dsd.general_from_dsd.export_offset_db", "None");
        }
        DsdGeneralExportLevel::ProtectedR64 => {
            writer.field_static("dsd.general_from_dsd.export_level", "protected_r64");
            writer.field_static("dsd.general_from_dsd.export_offset_db", "None");
        }
        DsdGeneralExportLevel::NativeWithOffset { offset_db } => {
            writer.field_static("dsd.general_from_dsd.export_level", "native_with_offset");
            writer.field_string("dsd.general_from_dsd.export_offset_db", offset_db.render(false));
        }
    }
    push_sample_gain_policy(writer, "dsd.general_from_dsd.gain", general.gain);
    writer.field_string(
        "dsd.runtime_album_gain_db",
        option_db_nano(settings.runtime_album_gain_db()),
    );
}

fn push_sinc(writer: &mut FingerprintWriter, settings: &SincFilterSettings) {
    writer.field_string(
        "dsd.pcm_to_dsd.sinc.oversample_factor",
        settings.oversample_factor.to_string(),
    );
    writer.field_string("dsd.pcm_to_dsd.sinc.taps", settings.taps.to_string());
    writer.field_string("dsd.pcm_to_dsd.sinc.passband_hz", f32_value(settings.passband_hz));
    writer.field_string("dsd.pcm_to_dsd.sinc.transition_hz", f32_value(settings.transition_hz));
    writer.field_string("dsd.pcm_to_dsd.sinc.kaiser_beta", f32_value(settings.kaiser_beta));
    writer.field_static("dsd.pcm_to_dsd.sinc.linear_phase", bool_value(settings.linear_phase));
    writer.field_static("dsd.pcm_to_dsd.sinc.allow_aliasing", bool_value(settings.allow_aliasing));
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
        option_static(settings.logical_mode().map(replay_gain_mode)),
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

fn true_peak_scope(value: crate::TruePeakScope) -> &'static str {
    match value {
        crate::TruePeakScope::Track => "track",
        crate::TruePeakScope::Album => "album",
    }
}

fn true_peak_scan(value: crate::TruePeakScanTier) -> &'static str {
    match value {
        crate::TruePeakScanTier::Reference => "fast066v2_reference",
        crate::TruePeakScanTier::Standard => "fast066v2_standard",
        crate::TruePeakScanTier::Fast => "fast066v2_fast",
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
            common_runtime_closure_fingerprint_sha256:
                Sha256Digest::of_bytes(b"common-closure").to_hex(),
        }
    }

    #[test]
    fn gain_mode_scope_tier_and_runtime_scalar_are_identity_dimensions() {
        let target = crate::settings::PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP;
        let off = fingerprint_with(|_| {});
        let guard_track_fast = fingerprint_with(|settings| {
            settings.pcm_true_peak.set_policy(SampleGainPolicy::TruePeakGuard {
                target_dbtp: target,
                scope: crate::TruePeakScope::Track,
                scan: crate::TruePeakScanTier::Fast,
            });
        });
        let normalize_track_fast = fingerprint_with(|settings| {
            settings.pcm_true_peak.set_policy(SampleGainPolicy::TruePeakNormalize {
                target_dbtp: target,
                scope: crate::TruePeakScope::Track,
                scan: crate::TruePeakScanTier::Fast,
            });
        });
        let guard_album_reference = fingerprint_with(|settings| {
            settings.pcm_true_peak.set_policy(SampleGainPolicy::TruePeakGuard {
                target_dbtp: target,
                scope: crate::TruePeakScope::Album,
                scan: crate::TruePeakScanTier::Reference,
            });
            settings.pcm_true_peak.set_runtime_album_gain_db(Some(DbNano(-1_000_000_000)));
        });
        assert_ne!(off, guard_track_fast);
        assert_ne!(guard_track_fast, normalize_track_fast);
        assert_ne!(guard_track_fast, guard_album_reference);
    }

    #[test]
    fn dsd_directional_reconstruction_and_export_level_are_identity_dimensions() {
        let general = fingerprint_with(|_| {});
        let protected = fingerprint_with(|settings| {
            settings.dsd.general_from_dsd.reconstruction = DsdGeneralReconstruction::ReferenceProtected;
        });
        let protected_native_offset = fingerprint_with(|settings| {
            settings.dsd.general_from_dsd.reconstruction = DsdGeneralReconstruction::ReferenceProtected;
            settings.dsd.general_from_dsd.export_level = DsdGeneralExportLevel::NativeWithOffset {
                offset_db: DbNano(500_000_000),
            };
        });
        assert_ne!(general, protected);
        assert_ne!(protected, protected_native_offset);
    }

    #[test]
    fn dither_explicit_is_mode_scoped_in_the_settings_fingerprint() {
        let automatic = fingerprint_with(|settings| {
            settings.dither_type = DitherType::Tpdf;
            settings.dither_explicit = false;
        });
        let explicit = fingerprint_with(|settings| {
            settings.dither_type = DitherType::Tpdf;
            settings.dither_explicit = true;
        });
        assert_ne!(automatic, explicit);
    }

    #[test]
    fn registered_effects_extend_ordinary_settings_identity_without_changing_empty_chain() {
        let settings = PipelineSettings::default();
        assert_eq!(
            settings_and_effects_fingerprint(&settings, &[]),
            settings_fingerprint(&settings),
        );

        let first = EffectIntent {
            id: crate::semantic_plan::EffectInstanceId(1),
            effect: RegisteredUnaryEffect::SoxHighPass { frequency_hz: 20 },
            after: Vec::new(),
            placement: crate::semantic_plan::EffectPlacement::AfterPcmResample,
        };
        let changed = EffectIntent {
            id: crate::semantic_plan::EffectInstanceId(1),
            effect: RegisteredUnaryEffect::SoxHighPass { frequency_hz: 30 },
            after: Vec::new(),
            placement: crate::semantic_plan::EffectPlacement::AfterPcmResample,
        };
        assert_ne!(
            settings_and_effects_fingerprint(&settings, &[first]),
            settings_and_effects_fingerprint(&settings, &[changed]),
        );
    }

    #[test]
    fn before_resample_effect_extends_historical_after_effect_identity() {
        let settings = PipelineSettings::default();
        let after = EffectIntent {
            id: crate::semantic_plan::EffectInstanceId(1),
            effect: RegisteredUnaryEffect::SoxHighPass { frequency_hz: 20 },
            after: Vec::new(),
            placement: crate::semantic_plan::EffectPlacement::AfterPcmResample,
        };
        let mut before = after.clone();
        before.placement = crate::semantic_plan::EffectPlacement::BeforePcmResample;
        assert_ne!(
            settings_and_effects_fingerprint(&settings, &[after]),
            settings_and_effects_fingerprint(&settings, &[before]),
        );
    }

    #[test]
    fn execution_fingerprint_binds_every_metadata_mutator_identity_component() {
        let behavior = BehaviorFingerprintV1(Sha256Digest::of_bytes(b"behavior"));
        let semantic = SemanticPlanHashV1(Sha256Digest::of_bytes(b"semantic"));
        let qualification = Sha256Digest::of_bytes(b"qualification");
        let base = test_reference_execution_identity();
        let base_fingerprint = execution_fingerprint_v1(behavior, semantic, qualification, &base);
        let mut changed = base.clone();
        changed.metadata_mutators.as_mut().unwrap().metaflac.reported_version.push_str("-changed");
        assert_ne!(
            base_fingerprint,
            execution_fingerprint_v1(behavior, semantic, qualification, &changed),
        );
    }
}
