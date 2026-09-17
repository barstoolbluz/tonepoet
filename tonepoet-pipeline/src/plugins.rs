//! Built-in tool plugins.
//!
//! The builders here are fresh implementations against the unified planning
//! types. They use the reference files only for argument-shape semantics.

use crate::enums::{
    AudioFormat, BitDepthTarget, DitherType, DsdFilterPreset, DsdLowpassMethod,
    GainCompensation,
    Mp3Mode, PcmBitDepth, SoxSincPhase, SsrcPdfType,
};
use crate::error::{PlanningError, Result};
use crate::mapping;
use crate::plan::{
    InputSource, MetadataPlanEffect, OutputSink, PlanContext, PlanOperation, PlanRequest, PlanStep,
    PlannedCommand,
};
use crate::tools::{MetadataDisposition, ToolIdentifier, ToolPlugin, ToolSupport};

/// Whether SSRC terminal dither is active for the selected immediate output.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SsrcDitherAvailability {
    /// Floating/nonterminal output, or an integer terminal with no requested dither.
    Inactive,
    /// SSRC owns the resolved native dither/PDF pair.
    Active,
    /// A derived global family has no native SSRC mapping for this terminal cell.
    /// The semantic planner may split to a later terminal when no explicit native
    /// override requires SSRC ownership.
    UnavailableForSsrcTerminal {
        /// Explanation of why the requested terminal cell has no native mapping.
        reason: String,
    },
}

/// Provenance of the effective SSRC dither selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SsrcDitherOrigin {
    /// No SSRC dither is active.
    None,
    /// The global dither maps exactly to the emitted native pair.
    GlobalExact,
    /// The global named family is represented by the documented SSRC approximation.
    GlobalApproximation,
    /// One or both SSRC-native fields were explicitly overridden.
    NativeOverride,
}

/// Effective SSRC terminal dither/PDF pair after rate/output-role resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ResolvedSsrcDither {
    /// Global dither value requested by the pipeline settings.
    pub requested_global: DitherType,
    /// Resolved SSRC dither identifier.
    pub dither_id: Option<u8>,
    /// Resolved SSRC probability-density-function setting.
    pub pdf_type: Option<SsrcPdfType>,
    /// Configuration source that supplied the resolved dither setting.
    pub origin: SsrcDitherOrigin,
    /// Availability result for the resolved dither setting.
    pub availability: SsrcDitherAvailability,
}

impl ResolvedSsrcDither {
    fn inactive(requested_global: DitherType) -> Self {
        Self {
            requested_global,
            dither_id: None,
            pdf_type: None,
            origin: SsrcDitherOrigin::None,
            availability: SsrcDitherAvailability::Inactive,
        }
    }
}

/// Resolve the exact SSRC dither/PDF pair used by command lowering.
///
/// Dither belongs to the final integer write, not to a comparison between the
/// source and destination nominal widths. Float/nonterminal SSRC output never
/// emits dither. A derived global family that is unavailable at the selected
/// rate is returned as a split-capable terminal miss; an active explicit native
/// override remains fail-closed.
pub(crate) fn resolve_ssrc_dither_for_rate(
    settings: &crate::settings::PipelineSettings,
    _source_depth: Option<PcmBitDepth>,
    effective_target_depth: Option<PcmBitDepth>,
    target_rate_hz: u32,
) -> Result<ResolvedSsrcDither> {
    let requested_global = settings.dither_type;
    let Some(target_depth) = effective_target_depth else {
        return Ok(ResolvedSsrcDither::inactive(requested_global));
    };
    if target_depth.is_float() {
        return Ok(ResolvedSsrcDither::inactive(requested_global));
    }

    let explicit_id = settings.ssrc.dither_id;
    let explicit_pdf = settings.ssrc.pdf_type;
    let native_override_active = explicit_id.is_some() || explicit_pdf.is_some();

    // Int32 native dither ownership remains behind the retained exact-cell
    // characterization gate. The semantic planner splits an explicit global
    // Int32 request to Float64 + the admitted later terminal; an explicit
    // SSRC-native override is refused before lowering because it cannot be
    // reassigned. A directly constructed Int32 SSRC step therefore never
    // emits uncommissioned native dither.
    if target_depth == PcmBitDepth::Int32 {
        if native_override_active || (settings.dither_explicit && requested_global != DitherType::None) {
            return Ok(ResolvedSsrcDither {
                requested_global,
                dither_id: None,
                pdf_type: None,
                origin: if native_override_active {
                    SsrcDitherOrigin::NativeOverride
                } else {
                    SsrcDitherOrigin::GlobalExact
                },
                availability: SsrcDitherAvailability::UnavailableForSsrcTerminal {
                    reason: "SSRC Int32 dither ownership is not commissioned for the retained pinned cell".to_owned(),
                },
            });
        }
        return Ok(ResolvedSsrcDither::inactive(requested_global));
    }

    if !native_override_active && requested_global == DitherType::None {
        return Ok(ResolvedSsrcDither::inactive(requested_global));
    }

    if native_override_active {
        let (dither_id, pdf_type) = match (explicit_id, explicit_pdf) {
            (Some(id), Some(pdf)) => (id, Some(pdf)),
            (Some(id), None) => {
                let pdf = if requested_global == DitherType::None {
                    None
                } else {
                    mapping::ssrc_dither_selection(requested_global).pdf_type
                };
                (id, pdf)
            }
            (None, Some(pdf)) if requested_global == DitherType::None => (99, Some(pdf)),
            (None, Some(pdf)) => {
                let mapped = mapping::ssrc_dither_selection_for_rate(
                    requested_global,
                    target_rate_hz,
                )?;
                (mapped.dither_id, Some(pdf))
            }
            (None, None) => unreachable!("native override branch requires one override"),
        };
        mapping::validate_ssrc_dither_id_for_rate(dither_id, target_rate_hz)?;
        return Ok(ResolvedSsrcDither {
            requested_global,
            dither_id: Some(dither_id),
            pdf_type,
            origin: SsrcDitherOrigin::NativeOverride,
            availability: SsrcDitherAvailability::Active,
        });
    }

    match mapping::ssrc_dither_selection_for_rate(requested_global, target_rate_hz) {
        Ok(mapped) => Ok(ResolvedSsrcDither {
            requested_global,
            dither_id: Some(mapped.dither_id),
            pdf_type: mapped.pdf_type,
            origin: if mapping::ssrc_dither_selection_is_approximation(requested_global) {
                SsrcDitherOrigin::GlobalApproximation
            } else {
                SsrcDitherOrigin::GlobalExact
            },
            availability: SsrcDitherAvailability::Active,
        }),
        Err(error) => Ok(ResolvedSsrcDither {
            requested_global,
            dither_id: None,
            pdf_type: None,
            origin: if mapping::ssrc_dither_selection_is_approximation(requested_global) {
                SsrcDitherOrigin::GlobalApproximation
            } else {
                SsrcDitherOrigin::GlobalExact
            },
            availability: SsrcDitherAvailability::UnavailableForSsrcTerminal {
                reason: error.to_string(),
            },
        }),
    }
}

fn parse_rendered_f32(rendered: &str) -> f32 {
    rendered
        .parse::<f32>()
        .expect("parameter renderer must emit a parseable f32")
}

/// Render the SSRC attenuation exactly as the command lowerer passes it.
#[must_use]
pub(crate) fn render_ssrc_attenuation_db(value: f32) -> String {
    format!("{value:.1}")
}

/// Effective SSRC attenuation after argv precision is applied.
#[must_use]
pub(crate) fn canonicalize_ssrc_attenuation_db(value: f32) -> f32 {
    parse_rendered_f32(&render_ssrc_attenuation_db(value))
}

/// Render the FFmpeg/SoXR cutoff exactly as the `aresample` option does.
#[must_use]
pub(crate) fn render_soxr_cutoff(value: f32) -> String {
    format!("{value:.3}")
}

/// Effective SoXR cutoff after argv precision is applied.
#[must_use]
pub(crate) fn canonicalize_soxr_cutoff(value: f32) -> f32 {
    parse_rendered_f32(&render_soxr_cutoff(value))
}

/// Render a SoX sinc passband frequency at the whole-Hz precision used by the
/// retained command path. The caller owns the leading low-pass `-` marker.
#[must_use]
pub(crate) fn render_sox_sinc_passband_hz(value: f32) -> String {
    format!("{value:.0}")
}

/// Effective SoX sinc passband after whole-Hz argv precision is applied.
#[must_use]
pub(crate) fn canonicalize_sox_sinc_passband_hz(value: f32) -> f32 {
    parse_rendered_f32(&render_sox_sinc_passband_hz(value))
}

/// Render SoX scalar controls at the retained three-decimal precision.
#[must_use]
pub(crate) fn render_sox_scalar(value: f32) -> String {
    let mut rendered = format!("{value:.3}");
    while rendered.contains('.') && rendered.ends_with('0') {
        rendered.pop();
    }
    if rendered.ends_with('.') {
        rendered.pop();
    }
    rendered
}

/// Effective SoX scalar after three-decimal argv precision is applied.
#[must_use]
pub(crate) fn canonicalize_sox_scalar(value: f32) -> f32 {
    parse_rendered_f32(&render_sox_scalar(value))
}

/// Render SoX gain compensation in signed dB at the retained two-decimal precision.
#[must_use]
pub(crate) fn render_sox_gain_db(value: f32) -> String {
    format!("{value:+.2}")
}

/// Effective SoX dB gain after argv precision is applied.
#[must_use]
pub(crate) fn canonicalize_sox_gain_db(value: f32) -> f32 {
    parse_rendered_f32(&render_sox_gain_db(value))
}

/// Exact FFmpeg/SoXR rate options after quality and transition defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ResolvedSoxrRateOptions {
    /// Resolved libsoxr precision value.
    pub precision: u8,
    /// Resolved libsoxr cutoff value.
    pub cutoff: f32,
    /// Whether Chebyshev passband behavior is enabled.
    pub chebyshev: bool,
    /// Resolved libsoxr phase value, when configured.
    pub phase: Option<u8>,
}

/// Resolve the SoXR options shared by the typed plan and FFmpeg lowering.
#[must_use]
pub(crate) fn resolve_soxr_rate_options(
    settings: &crate::settings::PipelineSettings,
) -> ResolvedSoxrRateOptions {
    let requested_cutoff = settings
        .soxr_resampler
        .cutoff
        .unwrap_or_else(|| mapping::ffmpeg_cutoff(settings.nyquist_transition));
    ResolvedSoxrRateOptions {
        precision: mapping::soxr_precision(settings.resample_quality),
        cutoff: canonicalize_soxr_cutoff(requested_cutoff),
        chebyshev: settings.soxr_resampler.chebyshev,
        phase: settings.soxr_resampler.phase,
    }
}

/// FFmpeg plugin.
#[derive(Debug, Default, Clone, Copy)]
pub struct FfmpegPlugin;

/// SoX plugin.
#[derive(Debug, Default, Clone, Copy)]
pub struct SoxPlugin;

/// SSRC plugin.
#[derive(Debug, Default, Clone, Copy)]
pub struct SsrcPlugin;

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
                target_bit_depth,
                apply_processing,
                ..
            } => {
                // ffmpeg's wavpack encoder has no 24-bit sample format: an
                // Int24 request stores true 32-bit ints (verified with
                // wvunpack -s) — the same silent-substitution class as sox
                // FLAC Int32. Hybrid requests are exempt because
                // build_ffmpeg_encode_pcm delegates them to the native
                // wavpack CLI, which writes true 24-bit.
                if matches!(
                    (target_format, target_bit_depth),
                    (AudioFormat::WavPack, PcmBitDepth::Int24)
                ) && !context.request.settings.wavpack.hybrid
                {
                    return ToolSupport::UNSUPPORTED;
                }
                let dither = effective_pcm_dither(context.request, Some(*target_bit_depth));
                // A bound DSD album gain or enabled PCM true-peak gain is a
                // hard-ceiling path. For integer dithered lossless output,
                // route the terminal stage to SoX so the deterministic
                // dither/noise-shaping support bound
                // used by the resolver matches the implementation that writes
                // the samples. This is ordinary planner selection, not runtime
                // commissioning or executable fingerprinting.
                let qualified_wavpack_hybrid_int32_preterminal = *target_format
                    == AudioFormat::Wav
                    && *target_bit_depth == PcmBitDepth::Int32
                    && *apply_processing
                    && wavpack_hybrid_float_source_int32_tpdf_requested(
                        context.request,
                        Some(PcmBitDepth::Int32),
                    );
                // A float-PCM WavPack-hybrid Source request is realized as an
                // Int32 WAV preterminal followed by native `wavpack`
                // packaging. The undithered cell (Float32, or an explicit
                // dither override to None) is deliberately SoX-owned. The
                // qualified FFmpeg/SoXR Int32 triangular preterminal is admitted
                // only when TPDF is required by Source policy or explicitly
                // selected by the user for this float-Source hybrid cell. This keeps
                // physical command ownership aligned with the semantic
                // terminal proof even when a retained-carrier bridge carries
                // an FFmpeg preference for the compound package candidate.
                let wavpack_hybrid_float_source_int32_preterminal = context
                    .request
                    .settings
                    .target_format
                    == AudioFormat::WavPack
                    && context.request.settings.wavpack.hybrid
                    && context.request.settings.target_bit_depth == BitDepthTarget::Source
                    && context
                        .request
                        .source
                        .authoritative_pcm_depth()
                        .is_some_and(PcmBitDepth::is_float)
                    && *target_format == AudioFormat::Wav
                    && *target_bit_depth == PcmBitDepth::Int32
                    && *apply_processing;
                if wavpack_hybrid_float_source_int32_preterminal
                    && !qualified_wavpack_hybrid_int32_preterminal
                {
                    return ToolSupport::UNSUPPORTED;
                }
                let hard_ceiling_requires_sox_terminal = (context
                    .request
                    .settings
                    .dsd
                    .runtime_album_gain_db()
                    .is_some()
                    || context.request.settings.pcm_true_peak.is_true_peak())
                    && *apply_processing
                    && target_format.is_pcm_lossless()
                    && target_format.sox_encodable()
                    && target_depth_needs_dither(*target_bit_depth)
                    && dither != DitherType::None
                    && !qualified_wavpack_hybrid_int32_preterminal;
                if hard_ceiling_requires_sox_terminal {
                    return ToolSupport::UNSUPPORTED;
                }
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

    fn metadata_effect(&self, context: &PlanContext<'_>, step: &PlanStep) -> MetadataPlanEffect {
        match &step.operation {
            PlanOperation::EncodePcm { target_format, .. }
            | PlanOperation::EncodeLossy { target_format, .. } => {
                ffmpeg_encode_metadata_effect(context, step, target_format)
            }
            PlanOperation::MetadataTransfer {
                target_format,
                transfer_tags,
                preserve_artwork,
            } if ffmpeg_metadata_transfer_supported(
                target_format,
                *transfer_tags,
                *preserve_artwork,
            ) => MetadataPlanEffect {
                source_tags_transferred_from_original_source: *transfer_tags,
                artwork_transferred_from_original_source: *preserve_artwork,
                ..MetadataPlanEffect::none()
            },
            _ => MetadataPlanEffect::none(),
        }
    }

    fn metadata_disposition(
        &self,
        context: &PlanContext<'_>,
        step: &PlanStep,
    ) -> MetadataDisposition {
        let effect = self.metadata_effect(context, step);
        match &step.operation {
            PlanOperation::EncodePcm { target_format, .. }
            | PlanOperation::EncodeLossy { target_format, .. }
                if ffmpeg_encoder_transfers_original_source_metadata(context, step, target_format) =>
            {
                MetadataDisposition::WritesRequestedPolicy
            }
            PlanOperation::MetadataTransfer { .. }
                if effect.source_tags_transferred_from_original_source
                    || effect.artwork_transferred_from_original_source =>
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
                target_bit_depth,
                apply_processing,
                ..
            } if target_format.sox_encodable() => {
                if *target_format == AudioFormat::WavPack
                    && context.request.settings.wavpack.hybrid
                    && (context.request.settings.pcm_true_peak.is_true_peak()
                        || context.request.settings.pcm_true_peak.fixed_gain_db().is_some())
                {
                    // PCM gain + WavPack hybrid deliberately realizes the
                    // final integer encoder-input WAV before this step. The
                    // terminal encode must therefore be native `wavpack -b`;
                    // the FFmpeg plugin owns that native-CLI delegation.
                    return ToolSupport::UNSUPPORTED;
                }
                let target_depth = *target_bit_depth;
                // Ordinary SoX Int32 output cannot realize the FFmpeg Int32
                // dither semantics selected by the typed planner, whether
                // explicitly requested or required by Source-depth policy.
                if int32_dither_requested(context.request, Some(target_depth)) {
                    return ToolSupport::UNSUPPORTED;
                }
                let silently_substituted = matches!(
                    ((*target_format).clone(), target_depth),
                    (AudioFormat::Flac, PcmBitDepth::Int32)
                        | (AudioFormat::Aiff, PcmBitDepth::Float32 | PcmBitDepth::Float64)
                        | (AudioFormat::WavPack, PcmBitDepth::Float32 | PcmBitDepth::Float64)
                );
                if silently_substituted {
                    return ToolSupport::UNSUPPORTED;
                }
                if *apply_processing
                    && mapping::requires_sox_dither(effective_pcm_dither(
                        context.request,
                        Some(target_depth),
                    ))
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
            // SSRC treats `--dither` and `--pdf` as parameters of one terminal
            // integer dither/quantization stage. Resolve the pair through the
            // same pure helper consumed by the common semantic planner.
            let resolved_dither = resolve_ssrc_dither_for_rate(
                &context.request.settings,
                context.request.source.authoritative_pcm_depth(),
                effective_target_depth(context, *target_bit_depth),
                *target_rate_hz,
            )?;
            if let SsrcDitherAvailability::UnavailableForSsrcTerminal { reason } =
                &resolved_dither.availability
            {
                return Err(PlanningError::plugin_rejected(
                    self.id(),
                    format!(
                        "selected SSRC terminal dither mapping is unavailable for destination rate {} Hz: {}",
                        target_rate_hz, reason
                    ),
                ));
            }
            if let Some(dither_id) = resolved_dither.dither_id {
                args.push("--dither".into());
                args.push(dither_id.to_string());
            }
            if let Some(depth) = *target_bit_depth {
                args.push("--bits".into());
                args.push(ssrc_bits_arg(depth));
            }
            if let Some(att) = context.request.settings.ssrc.attenuation_db {
                args.push("--att".into());
                args.push(render_ssrc_attenuation_db(att));
            }
            if context.request.settings.ssrc.min_phase {
                args.push("--minPhase".into());
            }
            if let Some(pdf) = resolved_dither.pdf_type {
                args.push("--pdf".into());
                args.push(match pdf {
                    SsrcPdfType::Rectangular => "0".into(),
                    SsrcPdfType::Triangular => "1".into(),
                });
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

/// Render the SSRC bit-depth argument for a resolved PCM depth.
pub(crate) fn ssrc_bits_arg(depth: PcmBitDepth) -> String {
    match depth {
        PcmBitDepth::Float32 => "-32".into(),
        PcmBitDepth::Float64 => "-64".into(),
        _ => depth.bits().to_string(),
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

    fn metadata_effect(&self, _context: &PlanContext<'_>, step: &PlanStep) -> MetadataPlanEffect {
        match &step.operation {
            PlanOperation::StoreSourceAudioMd5 { target_format }
                if target_format == &AudioFormat::Flac =>
            {
                MetadataPlanEffect {
                    source_audio_md5_written: true,
                    ..MetadataPlanEffect::none()
                }
            }
            _ => MetadataPlanEffect::none(),
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
                )
                .with_metadata_effect(self.metadata_effect(context, step)))
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
    validate_aac_family_container(context, target_format)?;
    let input = required_input_path(step)?;
    let output = required_output_path(step)?;
    let mut args = ffmpeg_base_input_args(context, step, &input)?;
    add_ffmpeg_metadata_args(context, &mut args, target_format);
    if apply_processing {
        add_ffmpeg_audio_filter_args(context, &mut args, target_rate_hz, Some(target_depth))?;
    }
    add_ffmpeg_pcm_encoder_args(context, &mut args, target_format, target_depth)?;
    add_ffmpeg_container_format_args(&mut args, target_format);
    add_ffmpeg_container_flags(context, &mut args);
    args.push(output);
    Ok(PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    )
    .with_metadata_effect(ffmpeg_encode_metadata_effect(context, step, target_format)))
}

fn build_ffmpeg_encode_lossy(
    context: &PlanContext<'_>,
    step: &PlanStep,
    target_format: &AudioFormat,
    target_rate_hz: Option<u32>,
    apply_processing: bool,
) -> Result<PlannedCommand> {
    validate_aac_family_container(context, target_format)?;
    if context.request.settings.dsd.runtime_album_gain_db().is_some() {
        let carrier_rate_hz = context.request.source.sample_rate_hz.ok_or_else(|| {
            PlanningError::invalid_source(
                "sample_rate_hz",
                "runtime DSD album gain lossy encode requires an authoritative carrier sample rate",
            )
        })?;
        if target_rate_hz != Some(carrier_rate_hz) {
            return Err(PlanningError::invalid_source(
                "sample_rate_hz",
                "runtime DSD album gain lossy encode must pin the encoder-input rate to the measured carrier rate",
            ));
        }
        if mapping::ffmpeg_lossy_encoder_accepts_rate_directly(target_format, carrier_rate_hz)
            != Some(true)
        {
            return Err(PlanningError::invalid_settings(
                "target_sample_rate",
                format!(
                    "runtime DSD album gain requires {} to accept {} Hz directly; refusing FFmpeg rate conversion after the proved gain",
                    target_format,
                    carrier_rate_hz,
                ),
            ));
        }
    }
    let encoder_rate_hz = target_rate_hz.ok_or_else(|| {
        PlanningError::invalid_settings(
            "target_sample_rate",
            format!(
                "{} encode requires an explicit resolved encoder-input sample rate; refusing to let FFmpeg choose or resample one implicitly",
                target_format,
            ),
        )
    })?;
    if mapping::ffmpeg_lossy_encoder_accepts_rate_directly(target_format, encoder_rate_hz)
        != Some(true)
    {
        return Err(PlanningError::invalid_settings(
            "target_sample_rate",
            format!(
                "{} does not accept {} Hz directly with Tonepoet's configured FFmpeg encoder; planner must resolve a supported rate before encoding",
                target_format,
                encoder_rate_hz,
            ),
        ));
    }
    let input = required_input_path(step)?;
    let output = required_output_path(step)?;
    let mut args = ffmpeg_base_input_args(context, step, &input)?;
    add_ffmpeg_metadata_args(context, &mut args, target_format);
    if apply_processing {
        let filter = ffmpeg_lossy_processing_filter(context, encoder_rate_hz);
        if !filter.is_empty() {
            args.push("-af".into());
            args.push(filter);
        }
    }
    args.push("-ar".into());
    args.push(encoder_rate_hz.to_string());
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
    add_ffmpeg_container_format_args(&mut args, target_format);
    add_ffmpeg_container_flags(context, &mut args);
    args.push(output);
    Ok(PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    )
    .with_metadata_effect(ffmpeg_encode_metadata_effect(context, step, target_format)))
}

fn ffmpeg_encode_metadata_effect(
    context: &PlanContext<'_>,
    step: &PlanStep,
    target_format: &AudioFormat,
) -> MetadataPlanEffect {
    let transfers_tags = context.request.settings.metadata.transfer_tags
        && format_supports_tags(target_format);
    let transfers_artwork = context.request.settings.metadata.preserve_artwork
        && format_supports_artwork(target_format);

    if ffmpeg_step_reads_original_request_input(context, step) {
        MetadataPlanEffect {
            source_tags_transferred_from_original_source: transfers_tags,
            artwork_transferred_from_original_source: transfers_artwork,
            ..MetadataPlanEffect::none()
        }
    } else {
        MetadataPlanEffect {
            tags_preserved_from_command_input: transfers_tags,
            artwork_preserved_from_command_input: transfers_artwork,
            ..MetadataPlanEffect::none()
        }
    }
}

fn ffmpeg_step_reads_original_request_input(context: &PlanContext<'_>, step: &PlanStep) -> bool {
    matches!(
        &step.input,
        crate::plan::InputSource::Path(path) if path == &context.request.input_path
    )
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

fn ffmpeg_encoder_transfers_original_source_metadata(
    context: &PlanContext<'_>,
    step: &PlanStep,
    format: &AudioFormat,
) -> bool {
    ffmpeg_step_reads_original_request_input(context, step)
        && ffmpeg_encoder_writes_requested_metadata_policy(context, format)
}

impl AudioFormat {
    /// True when the built-in planner/plugin metadata path can carry
    /// source-container text tags into this target format.
    ///
    /// This is the single capability definition used by both the planner
    /// plugins and the conversion bridge obligation model. Keeping it on the
    /// planner-owned format type prevents the bridge from mirroring FFmpeg
    /// support tables by hand.
    #[must_use]
    pub fn supports_planner_source_tag_transfer(&self) -> bool {
        matches!(
            self,
            AudioFormat::Flac
                | AudioFormat::Wav
                | AudioFormat::Aiff
                | AudioFormat::WavPack
                | AudioFormat::Mp3
                | AudioFormat::Aac
                | AudioFormat::Alac
        )
    }

    /// True when the built-in planner/plugin metadata path can preserve
    /// embedded artwork/video streams for this target format.
    ///
    /// This is intentionally narrower than text-tag support. The conversion
    /// bridge must use this same planner-owned capability when deciding whether
    /// artwork preservation is a real obligation.
    #[must_use]
    pub fn supports_planner_embedded_artwork_transfer(&self) -> bool {
        matches!(
            self,
            AudioFormat::Flac
                | AudioFormat::Mp3
                | AudioFormat::Aac
                | AudioFormat::Alac
        )
    }

    /// True when the orchestrator-owned post-encode CUE artwork stage has a
    /// concrete writer for the target container. This is separate from planner
    /// source artwork transfer because CUE uses an audio-only staged WAV carrier
    /// plus an extracted artwork sidecar from the original image.
    #[must_use]
    pub fn supports_cue_post_encode_artwork_embedding(&self) -> bool {
        matches!(
            self,
            AudioFormat::Flac
                | AudioFormat::Mp3
                | AudioFormat::Aac
                | AudioFormat::Alac
                | AudioFormat::WavPack
        )
    }
}

fn format_supports_tags(format: &AudioFormat) -> bool {
    format.supports_planner_source_tag_transfer()
}

fn format_supports_artwork(format: &AudioFormat) -> bool {
    format.supports_planner_embedded_artwork_transfer()
}

fn build_ffmpeg_metadata_transfer(
    context: &PlanContext<'_>,
    step: &PlanStep,
    target_format: &AudioFormat,
    transfer_tags: bool,
    preserve_artwork: bool,
) -> Result<PlannedCommand> {
    let encoded_input = required_input_path(step)?;
    let output = required_output_path(step)?;
    build_ffmpeg_source_metadata_transfer_command(
        std::path::Path::new(&encoded_input),
        &context.request.input_path,
        std::path::Path::new(&output),
        target_format,
        &context.target_container_extension(),
        &context.request.container_ffmpeg_flags,
        transfer_tags,
        preserve_artwork,
        context.request.source.duration,
        step.description.clone(),
    )
    .map(|command| command.with_metadata_effect(FfmpegPlugin.metadata_effect(context, step)))
}

/// Build the canonical FFmpeg source-container metadata rewrite used by the
/// planner after an audio encode.
///
/// `metadata_input` is deliberately distinct from `encoded_input`: callers
/// may have an audio-only realized carrier while the original source remains
/// the authoritative tag/artwork container. The helper is filesystem-I/O free
/// and preserves the planner's exact stream-copy semantics.
pub fn build_ffmpeg_source_metadata_transfer_command(
    encoded_input: &std::path::Path,
    metadata_input: &std::path::Path,
    output: &std::path::Path,
    target_format: &AudioFormat,
    container_extension: &str,
    container_ffmpeg_flags: &[String],
    transfer_tags: bool,
    preserve_artwork: bool,
    expected_duration: Option<std::time::Duration>,
    description: impl Into<String>,
) -> Result<PlannedCommand> {
    if !ffmpeg_metadata_transfer_supported(target_format, transfer_tags, preserve_artwork) {
        return Err(PlanningError::unsupported_format(
            target_format.clone(),
            "FFmpeg metadata rewrite does not support the requested tag/artwork policy for this target format",
        ));
    }
    validate_aac_family_container_extension(
        &container_extension.trim_start_matches('.').to_ascii_lowercase(),
        target_format,
    )?;

    let mut args = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-nostdin".into(),
        "-i".into(),
        encoded_input.to_string_lossy().into_owned(),
    ];
    if transfer_tags || preserve_artwork {
        args.push("-i".into());
        args.push(metadata_input.to_string_lossy().into_owned());
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
    add_ffmpeg_container_format_args(&mut args, target_format);
    args.extend(container_ffmpeg_flags.iter().cloned());
    args.push(output.to_string_lossy().into_owned());

    Ok(PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        args,
        InputSource::Path(encoded_input.to_path_buf()),
        OutputSink::Path(output.to_path_buf()),
        expected_duration,
        description,
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
    let carrier_width_restoration_without_dither = apply_processing
        && context.request.settings.dither_type == DitherType::None
        && context.request.source.bit_depth == Some(PcmBitDepth::Int32)
        && context.request.source.authoritative_pcm_depth() == Some(target_depth)
        && matches!(target_depth, PcmBitDepth::Int16 | PcmBitDepth::Int24);
    let mut args = vec!["-S".into()];
    if carrier_width_restoration_without_dither {
        // SoX automatically inserts dither for some precision reductions (in
        // particular s32 -> 16-bit) even when no explicit `dither` effect is
        // present. A widened CUE carrier is not a real precision reduction:
        // its lower bits contain no source information, and DitherType::None
        // must remain literal. Disable only SoX's implicit dither for this
        // source-depth restoration cell; explicit depth changes retain the
        // established planner behavior.
        args.push("-D".into());
    }
    add_sox_input_args(context, step, &mut args, input)?;
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
    let mut args = vec!["-S".into()];
    add_sox_input_args(context, step, &mut args, input)?;
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
    let mut args = vec!["-S".into()];
    add_sox_input_args(context, step, &mut args, input)?;
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
    add_sox_dsd_to_pcm_effects(context, &mut args, target_rate_hz, target_depth, lowpass)?;
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
    add_sox_dsd_rate_change_effects(context, &mut args, target_rate, lowpass)?;
    Ok(PlannedCommand::new(
        ToolIdentifier::Sox,
        args,
        step.input.clone(),
        step.output.clone(),
        context.request.source.duration,
        step.description.clone(),
    ))
}

fn internal_raw_f64le_input(
    context: &PlanContext<'_>,
    step: &PlanStep,
) -> Result<Option<(u32, u16)>> {
    let input_is_request_source = matches!(
        step.input.as_path(),
        Some(path) if path == context.request.input_path.as_path()
    );
    let input_is_raw_f64le = step
        .input
        .as_path()
        .and_then(|path| path.extension())
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("f64le"));
    let source = &context.request.source;
    let retained_dsd_carrier = context.request.settings.dsd.runtime_album_gain_db().is_some()
        && source.representation_kind() == crate::source::SourceRepresentationKind::Dsd;
    // Identify the internal retained PCM carrier by its physical representation,
    // not by the absence of original-source semantics. Retained true-peak carriers
    // are raw Float64 even when `true_source_depth` deliberately preserves the
    // authoritative Float32/Float64 depth of the original PCM source.
    let retained_pcm_gain_carrier = source.representation_kind()
        == crate::source::SourceRepresentationKind::Pcm
        && source.format == AudioFormat::Wav
        && source.codec == crate::enums::AudioCodec::PcmFloat;
    if (!retained_dsd_carrier && !retained_pcm_gain_carrier)
        || source.bit_depth != Some(PcmBitDepth::Float64)
        || !input_is_request_source
        || !input_is_raw_f64le
    {
        return Ok(None);
    }

    let sample_rate_hz = source
        .sample_rate_hz
        .filter(|sample_rate_hz| *sample_rate_hz > 0)
        .ok_or_else(|| {
            PlanningError::invalid_source(
                "sample_rate_hz",
                "raw Float64 carrier requires an authoritative positive PCM sample rate",
            )
        })?;
    let channels = source
        .channels
        .filter(|channels| *channels > 0)
        .ok_or_else(|| {
            PlanningError::invalid_source(
                "channels",
                "raw Float64 carrier requires an authoritative positive channel count",
            )
        })?;
    Ok(Some((sample_rate_hz, channels)))
}

fn ffmpeg_base_input_args(
    context: &PlanContext<'_>,
    step: &PlanStep,
    input: &str,
) -> Result<Vec<String>> {
    let mut args = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-nostdin".into(),
    ];
    if let Some((sample_rate_hz, channels)) = internal_raw_f64le_input(context, step)? {
        args.extend([
            "-f".into(),
            "f64le".into(),
            "-ar".into(),
            sample_rate_hz.to_string(),
            "-ac".into(),
            channels.to_string(),
        ]);
    }
    args.extend([
        "-i".into(),
        input.into(),
        "-map".into(),
        "0:a:0".into(),
    ]);
    Ok(args)
}

fn add_sox_input_args(
    context: &PlanContext<'_>,
    step: &PlanStep,
    args: &mut Vec<String>,
    input: String,
) -> Result<()> {
    if let Some((sample_rate_hz, channels)) = internal_raw_f64le_input(context, step)? {
        args.extend([
            "-t".into(),
            "raw".into(),
            "-e".into(),
            "floating-point".into(),
            "-b".into(),
            "64".into(),
            "-L".into(),
            "-r".into(),
            sample_rate_hz.to_string(),
            "-c".into(),
            channels.to_string(),
        ]);
    }
    args.push(input);
    Ok(())
}

/// Insert extra ffmpeg flags for the selected container (e.g., `-rf64 auto`).
/// No-op when no container override is active.
fn add_ffmpeg_container_flags(context: &PlanContext<'_>, args: &mut Vec<String>) {
    for flag in &context.request.container_ffmpeg_flags {
        args.push(flag.clone());
    }
}

fn validate_aac_family_container(
    context: &PlanContext<'_>,
    target_format: &AudioFormat,
) -> Result<()> {
    validate_aac_family_container_extension(&context.target_container_extension(), target_format)
}

fn validate_aac_family_container_extension(
    extension: &str,
    target_format: &AudioFormat,
) -> Result<()> {
    match target_format {
        AudioFormat::Aac => match extension {
            "m4a" | "m4b" | "mp4" => Ok(()),
            "aac" => Err(PlanningError::invalid_settings(
                "output_path",
                "AAC output is muxed as MP4-family M4A/M4B/MP4 by this pipeline; raw .aac output is not implemented, so use .m4a/.m4b/.mp4 or add an explicit raw-AAC mode",
            )),
            _ => Err(PlanningError::invalid_settings(
                "output_path",
                "AAC output must use an .m4a, .m4b, or .mp4 container extension unless an explicit raw-AAC mode is implemented",
            )),
        },
        AudioFormat::Alac => match extension {
            "m4a" | "mp4" => Ok(()),
            _ => Err(PlanningError::invalid_settings(
                "output_path",
                "ALAC output must use an .m4a or .mp4 container extension",
            )),
        },
        _ => Ok(()),
    }
}

fn add_ffmpeg_container_format_args(args: &mut Vec<String>, target_format: &AudioFormat) {
    match target_format {
        // Use the MP4/iPod muxer for AAC-family M4A/M4B/MP4 outputs. A raw ADTS .aac
        // stream cannot carry the metadata and artwork contract required here.
        AudioFormat::Aac | AudioFormat::Alac => {
            args.push("-f".into());
            args.push("ipod".into());
        }
        _ => {}
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

fn ffmpeg_soxr_resample_options(
    context: &PlanContext<'_>,
    target_rate_hz: Option<u32>,
) -> Vec<String> {
    let resolved = resolve_soxr_rate_options(&context.request.settings);
    let mut opts = vec!["resampler=soxr".to_string()];
    if let Some(rate) = target_rate_hz {
        opts.push(format!("out_sample_rate={rate}"));
    }
    opts.push(format!("precision={}", resolved.precision));
    opts.push(format!("cutoff={}", render_soxr_cutoff(resolved.cutoff)));
    if resolved.chebyshev {
        opts.push("cheby=1".to_string());
    }
    if let Some(phase) = resolved.phase {
        opts.push(format!("phase_shift={phase}"));
    }
    opts
}

fn ffmpeg_lossy_processing_filter(context: &PlanContext<'_>, encoder_rate_hz: u32) -> String {
    let mut filters = Vec::new();
    if let Some(gain) = context.request.settings.dsd.runtime_album_gain_db() {
        filters.push(format!("volume={}dB:precision=double", gain.render(false)));
    }
    if let Some(gain) = context.request.settings.pcm_true_peak.fixed_gain_db() {
        filters.push(format!("volume={}dB:precision=double", gain.render(false)));
    }
    if context.request.source.sample_rate_hz != Some(encoder_rate_hz) {
        filters.push(format!(
            "aresample={}",
            ffmpeg_soxr_resample_options(context, Some(encoder_rate_hz)).join(":")
        ));
    }
    filters.join(",")
}

fn ffmpeg_audio_filter(
    context: &PlanContext<'_>,
    target_rate_hz: Option<u32>,
    target_depth: Option<PcmBitDepth>,
) -> Result<String> {
    let settings = &context.request.settings;
    let mut filters = Vec::new();
    if let Some(gain) = settings.dsd.runtime_album_gain_db() {
        filters.push(format!("volume={}dB:precision=double", gain.render(false)));
    }
    if let Some(gain) = settings.pcm_true_peak.fixed_gain_db() {
        filters.push(format!("volume={}dB:precision=double", gain.render(false)));
    }

    let processing_produces_fractional_pcm = target_rate_hz.is_some()
        || settings.dsd.runtime_album_gain_db().is_some()
        || settings.pcm_true_peak.fixed_gain_db().is_some();
    let effective_depth = effective_target_depth(context, target_depth);
    let dither = effective_pcm_dither(context.request, effective_depth);
    let ffmpeg_needs_dither = match effective_depth {
        Some(PcmBitDepth::Int32) => int32_dither_requested(context.request, effective_depth),
        Some(depth) => {
            target_depth_needs_dither(depth)
                && (processing_produces_fractional_pcm
                    || pcm_conversion_reduces_depth(
                        context.request.source.authoritative_pcm_depth(),
                        depth,
                    ))
        }
        None => false,
    };
    let aresample_needed =
        target_rate_hz.is_some() || target_depth.is_some() || dither != DitherType::None;
    if aresample_needed {
        let mut opts = ffmpeg_soxr_resample_options(context, target_rate_hz);
        if dither != DitherType::None {
            match mapping::soxr_dither_method(dither) {
                Some(method) if ffmpeg_needs_dither => {
                    opts.push(format!("dither_method={method}"));
                }
                None if ffmpeg_needs_dither => {
                    return Err(PlanningError::invalid_settings(
                        "dither_type",
                        "selected dither is not supported by FFmpeg/SoXR; select SoX or Auto",
                    ));
                }
                _ => {}
            }
        }
        if let Some(depth) = target_depth {
            opts.push(format!(
                "out_sample_fmt={}",
                mapping::ffmpeg_sample_fmt(depth)
            ));
        }
        filters.push(format!("aresample={}", opts.join(":")));
    }
    Ok(filters.join(","))
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
            if target_depth == PcmBitDepth::Int32 {
                // FFmpeg otherwise silently stores s32 input as 24-bit FLAC.
                // Experimental mode is the explicit opt-in for true 32-bit FLAC.
                args.push("-strict".into());
                args.push("experimental".into());
            }
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
    if context.request.settings.dsd.runtime_album_gain_db().is_some() {
        return Err(PlanningError::invalid_settings(
            "dsd.general_from_dsd.gain.scope",
            "album-scoped DSD true-peak gain is not available with WavPack hybrid output because the native hybrid encoder cannot apply the submitted-batch fixed-gain authority",
        ));
    }
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
    if let Some(gain) = context.request.settings.dsd.runtime_album_gain_db() {
        args.push("gain".into());
        args.push(gain.render(false));
    }
    if let Some(gain) = context.request.settings.pcm_true_peak.fixed_gain_db() {
        args.push("gain".into());
        args.push(gain.render(false));
    }
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
                args.push(format!("-{}", render_sox_sinc_passband_hz(pb))); // negative = lowpass
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
    let effective_depth = effective_target_depth(context, target_depth);
    // Ordinary SoX is not behavior-qualified for 32-bit integer dither. SoX
    // 14.4.2 accepts a trailing `dither` effect for s32 output while producing
    // byte-identical PCM, so command syntax is not evidence that the DSP stage
    // occurred. Keep the established <=24-bit reduction path, but fail closed
    // for Int32 until runtime tool identity and a successful behavior probe can
    // be bound to planning. The separately qualified DSD Reference path does
    // not use this ordinary builder.
    // This helper is called only for an active SoX processing stage. A rate
    // change or gain can make the terminal input fractional even when source
    // storage width equals target width; an otherwise unprocessed same-depth
    // integer path must retain the established no-dither behavior. The typed
    // terminal realization remains authoritative, and the plan/lowering
    // boundary checks this command against that resolved fact.
    let processing_produces_fractional_pcm = target_rate_hz.is_some()
        || context.request.settings.dsd.runtime_album_gain_db().is_some()
        || context.request.settings.pcm_true_peak.fixed_gain_db().is_some();
    let depth_allows_dither = match effective_depth {
        Some(depth) => {
            target_depth_needs_dither(depth)
                && (processing_produces_fractional_pcm
                    || pcm_conversion_reduces_depth(
                        context.request.source.authoritative_pcm_depth(),
                        depth,
                    ))
        }
        None => true,
    };
    let dither = effective_pcm_dither(context.request, effective_depth);
    let should_dither = dither != DitherType::None && depth_allows_dither;
    if should_dither {
        args.extend(mapping::sox_dither_args(dither));
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
            let sinc = context.request.settings.dsd.pcm_to_dsd.sinc;
            args.push("upsample".into());
            args.push(sinc.oversample_factor.to_string());
            args.push("sinc".into());
            args.push(format!("-{}", render_sox_sinc_passband_hz(sinc.passband_hz)));
            args.push("-n".into());
            args.push(sinc.taps.to_string());
            args.push("-t".into());
            args.push(render_sox_scalar(sinc.transition_hz));
            if sinc.linear_phase {
                args.push("-L".into());
            } else {
                args.push("-M".into());
            }
            args.push("-b".into());
            args.push(render_sox_scalar(sinc.kaiser_beta));
            if sinc.allow_aliasing {
                args.push("-a".into());
            }
            match context.request.settings.dsd.pcm_to_dsd.gain_compensation {
                GainCompensation::Auto => {
                    args.push("vol".into());
                    args.push(sinc.oversample_factor.to_string());
                }
                GainCompensation::Linear(value) => {
                    args.push("vol".into());
                    args.push(render_sox_scalar(value));
                }
                GainCompensation::Decibels(value) => {
                    args.push("gain".into());
                    args.push(render_sox_gain_db(value));
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
) -> Result<()> {
    match lowpass {
        DsdLowpassMethod::Sinc => {
            let sinc = context.request.settings.dsd.general_from_dsd.sinc;
            if sinc.allow_aliasing {
                return Err(PlanningError::invalid_settings(
                    "dsd.general_from_dsd.sinc.allow_aliasing",
                    "general DSD-to-PCM sinc alias permission is not supported by the retained SoX lowerer",
                ));
            }
            args.push("sinc".into());
            args.push(format!("-{}", render_sox_sinc_passband_hz(sinc.passband_hz)));
            args.push("-n".into());
            args.push(sinc.taps.to_string());
            args.push("-t".into());
            args.push(render_sox_scalar(sinc.transition_hz));
            if sinc.linear_phase {
                args.push("-L".into());
            } else {
                args.push("-M".into());
            }
            args.push("-b".into());
            args.push(render_sox_scalar(sinc.kaiser_beta));
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
                        // The sinc runs at the OUTPUT rate (it follows the
                        // rate effect). When the cutoff falls at/above the
                        // target Nyquist, sox rejects the filter outright —
                        // and the strip is redundant anyway: the rate
                        // converter's anti-alias filter already bandlimits
                        // to Nyquist (the same rationale that makes
                        // default_pcm_lowpass_hz None for DSD512/1024).
                        // Without this guard, DSD256 -> <192k, DSD128 ->
                        // <96k, and DSD64 -> 44.1/48k all failed with
                        // "sinc: filter frequency must be less than
                        // sample-rate / 2".
                        if u64::from(lowpass_hz) < u64::from(target_rate_hz) / 2 {
                            args.push("sinc".into());
                            args.push("-a".into());
                            args.push("180".into());
                            args.push(format!("-{lowpass_hz}"));
                        }
                    }
                }
            }
        }
    }
    add_sox_dsd_to_pcm_gain(&context.request.settings.dsd, args)?;
    // See add_sox_pcm_effects: ordinary SoX Int32 dither is deliberately
    // fail-closed because supported installations may accept the effect while
    // producing no sample-level change. Qualified DSD Reference processing is
    // a separate, identity-bound path.
    if target_depth_needs_dither(target_depth)
        && context.request.settings.dither_type != DitherType::None
    {
        args.extend(mapping::sox_dither_args(
            context.request.settings.dither_type,
        ));
    }
    Ok(())
}

fn effective_target_depth(
    context: &PlanContext<'_>,
    target_depth: Option<PcmBitDepth>,
) -> Option<PcmBitDepth> {
    target_depth.or_else(|| match context.request.settings.target_bit_depth {
        BitDepthTarget::Pcm(depth) => Some(depth),
        BitDepthTarget::Source => context
            .request
            .source
            .authoritative_pcm_depth()
            .map(|source_depth| {
                crate::settings::source_pcm_depth_for_target(
                    &context.request.settings.target_format,
                    context.request.settings.wavpack.hybrid,
                    source_depth,
                )
            }),
    })
}

/// True when Source-depth policy selects automatic TPDF for this concrete terminal.
///
/// An explicit user dither choice, including explicit `None`, remains authoritative.
/// The automatic rule applies only to authoritative PCM Source semantics and only
/// when the format-safe Source landing depth is narrower than the float source.
pub(crate) fn source_depth_policy_tpdf_requested(
    request: &PlanRequest,
    target_depth: Option<PcmBitDepth>,
) -> bool {
    if request.settings.target_bit_depth != BitDepthTarget::Source
        || request.settings.dither_explicit
        || request.source.representation_kind() != crate::source::SourceRepresentationKind::Pcm
    {
        return false;
    }
    let Some(source_depth) = request.source.authoritative_pcm_depth() else {
        return false;
    };
    if !crate::settings::source_depth_policy_requires_tpdf(
        &request.settings.target_format,
        request.settings.wavpack.hybrid,
        source_depth,
    ) {
        return false;
    }
    let effective = crate::settings::source_pcm_depth_for_target(
        &request.settings.target_format,
        request.settings.wavpack.hybrid,
        source_depth,
    );
    target_depth.map_or(true, |depth| depth == effective)
}

/// True for the one WavPack-hybrid float-Source Int32 cell whose TPDF
/// can be owned by the commissioned FFmpeg/SoXR preterminal.
///
/// This intentionally admits only authoritative Float32/Float64 PCM with a
/// `Source` depth request resolved to Int32, and only plain TPDF selected either
/// by the automatic Source-depth rule or explicitly by the user. It does not
/// commission arbitrary explicit Int32 dither, SoX/SSRC Int32 dither, or any
/// other WavPack-hybrid dither mode.
pub(crate) fn wavpack_hybrid_float_source_int32_tpdf_requested(
    request: &PlanRequest,
    target_depth: Option<PcmBitDepth>,
) -> bool {
    if target_depth != Some(PcmBitDepth::Int32)
        || request.settings.target_format != AudioFormat::WavPack
        || !request.settings.wavpack.hybrid
        || request.settings.target_bit_depth != BitDepthTarget::Source
        || request.source.representation_kind() != crate::source::SourceRepresentationKind::Pcm
        || !matches!(
            request.source.authoritative_pcm_depth(),
            Some(PcmBitDepth::Float32 | PcmBitDepth::Float64)
        )
    {
        return false;
    }

    let automatic = source_depth_policy_tpdf_requested(request, target_depth);
    let explicit = request.settings.dither_explicit
        && request.settings.dither_type == DitherType::Tpdf;
    (automatic || explicit) && effective_pcm_dither(request, target_depth) == DitherType::Tpdf
}

/// Dither algorithm physically required for the selected PCM terminal.
///
/// Configured dither retains precedence. If no algorithm is configured, the
/// Source-depth float-reduction rule contributes policy-selected TPDF.
pub(crate) fn effective_pcm_dither(
    request: &PlanRequest,
    target_depth: Option<PcmBitDepth>,
) -> DitherType {
    if request.settings.dither_type != DitherType::None {
        request.settings.dither_type
    } else if source_depth_policy_tpdf_requested(request, target_depth) {
        DitherType::Tpdf
    } else {
        DitherType::None
    }
}

/// Return whether Int32 dither ownership is required by either explicit policy
/// or the automatic Source-depth float-reduction rule.
pub(crate) fn int32_dither_requested(
    request: &PlanRequest,
    target_depth: Option<PcmBitDepth>,
) -> bool {
    target_depth == Some(PcmBitDepth::Int32)
        && (explicit_int32_dither_requested(&request.settings, target_depth)
            || source_depth_policy_tpdf_requested(request, target_depth))
}

/// Return whether the request explicitly assigns Int32 dither ownership.
pub(crate) fn explicit_int32_dither_requested(
    settings: &crate::settings::PipelineSettings,
    target_depth: Option<PcmBitDepth>,
) -> bool {
    settings.dither_explicit
        && settings.dither_type != DitherType::None
        && target_depth == Some(PcmBitDepth::Int32)
}

/// True for the ordinary automatic low-bit-depth dither policy.
///
/// This is deliberately not a universal backend capability predicate: Int32 is
/// excluded from automatic dither, while FFmpeg can still realize an explicit
/// Int32 dither request. Physical terminal behavior must come from the selected
/// terminal realization, not from this helper alone.
pub(crate) fn target_depth_needs_dither(depth: PcmBitDepth) -> bool {
    matches!(depth, PcmBitDepth::Int8 | PcmBitDepth::Int16 | PcmBitDepth::Int24)
}

/// True when a PCM→PCM conversion reduces precision into a target covered by
/// the ordinary automatic low-bit-depth dither policy. This does not describe
/// backend-specific explicit Int32 behavior. Returns true conservatively when
/// source depth is unknown.
pub(crate) fn pcm_conversion_reduces_depth(
    source_depth: Option<PcmBitDepth>,
    target_depth: PcmBitDepth,
) -> bool {
    if !target_depth_needs_dither(target_depth) {
        return false;
    }
    match source_depth {
        Some(source) => target_depth.bits() < source.bits(),
        None => true,
    }
}

fn add_sox_dsd_to_pcm_gain(
    dsd: &crate::settings::DsdSettings,
    args: &mut Vec<String>,
) -> Result<()> {
    use crate::settings::SampleGainPolicy;

    match dsd.gain_policy() {
        SampleGainPolicy::Off => Ok(()),
        SampleGainPolicy::FixedGain { gain_db } => {
            args.push("gain".into());
            args.push(gain_db.render(false));
            Ok(())
        }
        SampleGainPolicy::TruePeakGuard { .. } | SampleGainPolicy::TruePeakNormalize { .. } => {
            if let Some(gain_db) = dsd.runtime_album_gain_db() {
                // The submitted-batch coordinator resolved this scalar with the
                // shared terminal-aware solver.  Applying it is not a second
                // policy decision and never invokes SoX sample-peak `norm`.
                args.push("gain".into());
                args.push(gain_db.render(false));
                Ok(())
            } else {
                Err(PlanningError::capability_unavailable(
                    "dsd_certified_true_peak_gain",
                    "certified DSD Guard/Normalize requires the typed observation/decision route; Phase 2 must not lower it to SoX norm",
                ))
            }
        }
    }
}

fn add_sox_dsd_rate_change_effects(
    context: &PlanContext<'_>,
    args: &mut Vec<String>,
    target_rate: crate::enums::DsdRate,
    lowpass: DsdLowpassMethod,
) -> Result<()> {
    match lowpass {
        DsdLowpassMethod::Sinc => {
            let sinc = context.request.settings.dsd.general_from_dsd.sinc;
            if sinc.allow_aliasing {
                return Err(PlanningError::invalid_settings(
                    "dsd.general_from_dsd.sinc.allow_aliasing",
                    "general DSD-to-PCM sinc alias permission is not supported by the retained SoX lowerer",
                ));
            }
            args.push("sinc".into());
            args.push(format!("-{}", render_sox_sinc_passband_hz(sinc.passband_hz)));
            args.push("-n".into());
            args.push(sinc.taps.to_string());
            args.push("-t".into());
            args.push(render_sox_scalar(sinc.transition_hz));
            if sinc.linear_phase {
                args.push("-L".into());
            } else {
                args.push("-M".into());
            }
            args.push("-b".into());
            args.push(render_sox_scalar(sinc.kaiser_beta));
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
    Ok(())
}

fn add_sox_sdm_args(context: &PlanContext<'_>, args: &mut Vec<String>) {
    let dsd = context.request.settings.dsd;
    args.push("sdm".into());
    args.push("-f".into());
    args.push(mapping::dsd_shaper_name(
        dsd.pcm_to_dsd.noise_shaper,
        dsd.pcm_to_dsd.modulator_order,
    ));
    if let Some(trellis) = dsd.pcm_to_dsd.trellis {
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::plan::{InputSource, MetadataPlanEffect, OutputSink, PlanAction, PlanOperation, PlanRequest, PlanStep};
    use crate::settings::{DsdSettings, PipelineSettings, SampleGainPolicy, PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP};
    use crate::enums::SsrcPdfType;
    use crate::source::SourceInfo;
    use std::path::{Path, PathBuf};

    fn ssrc_resample_command_with(
        settings: PipelineSettings,
        target_rate_hz: u32,
        target_bit_depth: Option<PcmBitDepth>,
    ) -> Result<PlannedCommand> {
        let source = SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Wav,
            codec: crate::enums::AudioCodec::PcmSigned,
            sample_rate_hz: Some(44_100),
            bit_depth: Some(PcmBitDepth::Int24),
            true_source_depth: Some(PcmBitDepth::Int24),
            source_representation: Default::default(),
            sample_kind: Some(crate::enums::SampleKind::SignedInteger),
            channels: Some(2),
            duration: None,
            frame_extent: None,
            audio_md5: None,
        };
        let request = PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,

            input_path: PathBuf::from("source.wav"),
            output_path: PathBuf::from("output.wav"),
            source,
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        };
        let context = request.context();
        let step = PlanStep::new(
            0,
            PlanOperation::ResamplePcm {
                target_rate_hz,
                target_bit_depth,
                profile: None,
                brick_wall: true,
            },
            InputSource::Path(PathBuf::from("source.wav")),
            OutputSink::Path(PathBuf::from("output.wav")),
            "SSRC command-builder regression test",
        );

        SsrcPlugin.build_command(&context, &step)
    }

    fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| pair[1].as_str())
    }

    fn assert_arg(args: &[String], flag: &str, expected: &str) {
        assert_eq!(arg_value(args, flag), Some(expected), "missing or wrong {flag} in {args:?}");
    }

    fn assert_no_arg(args: &[String], flag: &str) {
        assert!(arg_value(args, flag).is_none(), "unexpected {flag} in {args:?}");
    }

    fn pcm_request_with(
        settings: PipelineSettings,
        source_depth: PcmBitDepth,
    ) -> PlanRequest {
        let source_is_float = source_depth.is_float();
        PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,
            input_path: PathBuf::from("source.wav"),
            output_path: PathBuf::from("output.wav"),
            source: SourceInfo {
                dsd_source_kind: None,
                format: AudioFormat::Wav,
                codec: if source_is_float {
                    crate::enums::AudioCodec::PcmFloat
                } else {
                    crate::enums::AudioCodec::PcmSigned
                },
                sample_rate_hz: Some(96_000),
                bit_depth: Some(source_depth),
                true_source_depth: Some(source_depth),
                source_representation: Default::default(),
                sample_kind: Some(if source_is_float {
                    crate::enums::SampleKind::Float
                } else {
                    crate::enums::SampleKind::SignedInteger
                }),
                channels: Some(2),
                duration: None,
                frame_extent: None,
                audio_md5: None,
            },
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        }
    }


    fn album_gain_pcm_request(target_format: AudioFormat) -> PlanRequest {
        let mut settings = PipelineSettings::default();
        settings.target_format = target_format.clone();
        settings.target_sample_rate = crate::enums::RateTarget::PcmHz(96_000);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
        settings.dsd.set_gain_policy(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: crate::enums::TruePeakScope::Album,
            scan: crate::enums::TruePeakScanTier::Fast,
        });
        settings.dsd.bind_runtime_album_gain(
            "2.125000000".parse().unwrap(),
            None,
            2,
        );
        let mut request = pcm_request_with(settings, PcmBitDepth::Float64);
        request.source.source_representation = crate::source::SourceRepresentationKind::Dsd;
        request.source.true_source_depth = None;
        request.input_path = PathBuf::from("album-carrier.f64le");
        request.output_path = match target_format {
            AudioFormat::Mp3 => PathBuf::from("output.mp3"),
            _ => PathBuf::from("output.flac"),
        };
        request
    }

    fn album_gain_encode_step(target_format: AudioFormat, apply_processing: bool) -> PlanStep {
        PlanStep::new(
            0,
            PlanOperation::EncodePcm {
                target_format,
                target_rate_hz: None,
                target_bit_depth: PcmBitDepth::Int24,
                apply_processing,
            },
            InputSource::Path(PathBuf::from("album-carrier.f64le")),
            OutputSink::Path(PathBuf::from("output.flac")),
            "album gain application test",
        )
    }

    #[test]
    fn ffmpeg_pcm_album_gain_is_owned_only_by_processing_encode_step() {
        let request = album_gain_pcm_request(AudioFormat::Flac);
        for (apply_processing, expected_count) in [(false, 0), (true, 1)] {
            let step = album_gain_encode_step(AudioFormat::Flac, apply_processing);
            let command = build_ffmpeg_encode_pcm(
                &request.context(),
                &step,
                &AudioFormat::Flac,
                None,
                PcmBitDepth::Int24,
                apply_processing,
            )
            .expect("FFmpeg album-gain encode command");
            let count = command
                .args
                .iter()
                .filter(|arg| arg.contains("volume=2.125000000dB"))
                .count();
            assert_eq!(count, expected_count, "{:?}", command.args);
            if apply_processing {
                // The -af value is a filter chain: terminal bit-depth/dither
                // realization is appended after the gain, at the same sample
                // rate, and is budgeted by the terminal bound. The invariant
                // that matters is that the gain is the FIRST link and runs in
                // double precision, so assert on the head of the chain rather
                // than on the whole argument.
                assert!(
                    command.args.iter().any(|arg| arg
                        .split(',')
                        .next()
                        .is_some_and(|head| head == "volume=2.125000000dB:precision=double")),
                    "album gain must execute in FFmpeg double precision: {:?}",
                    command.args,
                );
            }
        }
    }

    #[test]
    fn fixed_gain_same_depth_low_bit_terminal_uses_post_gain_dither_truth() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
        settings.dither_type = DitherType::Tpdf;
        settings.pcm_true_peak.set_policy(SampleGainPolicy::FixedGain {
            gain_db: "1.000000000".parse().expect("fixed gain"),
        });
        let mut request = pcm_request_with(settings, PcmBitDepth::Int24);
        request.output_path = PathBuf::from("output.flac");

        let typed = match crate::semantic_plan::plan_typed(&request)
            .expect("typed fixed-gain terminal plan")
        {
            crate::semantic_plan::PlanningOutcome::Ready(plan) => plan,
            other => panic!("fixed-gain terminal should be Ready: {other:?}"),
        };
        let realization = typed.nodes.iter().find_map(|node| match node {
            crate::semantic_plan::TypedPlanNode::Operation {
                operation: PlanOperation::EncodePcm { .. },
                candidates,
                selected_candidate,
                ..
            } => candidates.get(*selected_candidate).and_then(|candidate| {
                candidate.contract.terminal_realization.as_ref()
            }),
            _ => None,
        }).expect("selected fixed-gain terminal realization");
        let crate::semantic_plan::SelectedTerminalRealization::Pcm(realization) = realization else {
            panic!("expected PCM fixed-gain terminal realization")
        };
        assert_eq!(
            realization.input_value_domain,
            crate::semantic_plan::ValueDomain::FiniteFloating,
        );
        assert_eq!(realization.effective_dither, Some(DitherType::Tpdf));

        let lowered = crate::plan_conversion(&request)
            .expect("production fixed-gain command lowering must agree with typed terminal");
        let PlanAction::Execute { commands, .. } = lowered.action else {
            panic!("fixed gain conversion must execute")
        };
        let terminal = commands.iter().find(|command| command.tool == ToolIdentifier::Sox)
            .expect("SoX fixed-gain terminal command");
        assert!(terminal.args.iter().any(|arg| arg == "gain"), "{:?}", terminal.args);
        assert!(terminal.args.iter().any(|arg| arg == "dither"), "{:?}", terminal.args);
    }

    #[test]
    fn pcm_fixed_gain_is_emitted_once_as_a_plain_amplitude_change() {
        let mut request = pcm_request_with(PipelineSettings::default(), PcmBitDepth::Float64);
        request.settings.target_format = AudioFormat::Flac;
        request.settings.pcm_true_peak.set_policy(SampleGainPolicy::FixedGain {
            gain_db: "3.250000000".parse().expect("fixed gain"),
        });
        let step = album_gain_encode_step(AudioFormat::Flac, true);

        let ffmpeg = build_ffmpeg_encode_pcm(
            &request.context(),
            &step,
            &AudioFormat::Flac,
            None,
            PcmBitDepth::Int24,
            true,
        )
        .expect("FFmpeg fixed-gain command");
        let filter = arg_value(&ffmpeg.args, "-af").expect("FFmpeg fixed-gain filter");
        assert_eq!(
            filter.split(',').next(),
            Some("volume=3.250000000dB:precision=double"),
            "fixed gain must be the plain first amplitude transform: {filter}",
        );
        for forbidden in ["loudnorm", "alimiter", "acompressor", "dynaudnorm", "compand"] {
            assert!(!filter.contains(forbidden), "unexpected dynamic processing: {filter}");
        }

        let sox = build_sox_encode_pcm(
            &request.context(),
            &step,
            &AudioFormat::Flac,
            None,
            PcmBitDepth::Int24,
            true,
        )
        .expect("SoX fixed-gain command");
        assert!(
            sox.args.windows(2).any(|pair| pair == ["gain", "3.250000000"]),
            "SoX must receive the exact user gain: {:?}",
            sox.args,
        );
    }

    #[test]
    fn ffmpeg_ordinary_lossy_encode_refuses_missing_or_unsupported_rate_pins() {
        let mut request = pcm_request_with(PipelineSettings::default(), PcmBitDepth::Int24);
        request.settings.target_format = AudioFormat::Aac;
        request.output_path = PathBuf::from("output.m4a");
        let step = PlanStep::new(
            0,
            PlanOperation::EncodeLossy {
                target_format: AudioFormat::Aac,
                target_rate_hz: None,
                apply_processing: false,
            },
            InputSource::Path(PathBuf::from("input.wav")),
            OutputSink::Path(PathBuf::from("output.m4a")),
            "ordinary AAC rate-pin defense",
        );

        let missing = build_ffmpeg_encode_lossy(
            &request.context(),
            &step,
            &AudioFormat::Aac,
            None,
            false,
        )
        .expect_err("ordinary lossy encode must not delegate rate choice to FFmpeg");
        assert!(missing.to_string().contains("explicit resolved encoder-input sample rate"), "{missing}");

        let unsupported = build_ffmpeg_encode_lossy(
            &request.context(),
            &step,
            &AudioFormat::Aac,
            Some(192_000),
            false,
        )
        .expect_err("ordinary lossy encode must not pass an unsupported rate to FFmpeg");
        assert!(unsupported.to_string().contains("does not accept 192000 Hz directly"), "{unsupported}");
    }

    #[test]
    fn ffmpeg_ordinary_lossy_rate_change_uses_explicit_soxr_before_the_rate_pin() {
        let mut request = pcm_request_with(PipelineSettings::default(), PcmBitDepth::Int24);
        request.settings.target_format = AudioFormat::Opus;
        request.output_path = PathBuf::from("output.opus");
        let step = PlanStep::new(
            0,
            PlanOperation::EncodeLossy {
                target_format: AudioFormat::Opus,
                target_rate_hz: Some(48_000),
                apply_processing: true,
            },
            InputSource::Path(PathBuf::from("input.wav")),
            OutputSink::Path(PathBuf::from("output.opus")),
            "ordinary Opus explicit resample",
        );

        let command = build_ffmpeg_encode_lossy(
            &request.context(),
            &step,
            &AudioFormat::Opus,
            Some(48_000),
            true,
        )
        .expect("96 -> 48 kHz Opus must use Tonepoet's configured SoXR path");
        assert_arg(&command.args, "-ar", "48000");
        let filter = arg_value(&command.args, "-af").expect("explicit SoXR filter");
        assert!(filter.starts_with("aresample=resampler=soxr:"), "{filter}");
        assert!(filter.contains("out_sample_rate=48000"), "{filter}");
        assert!(!filter.contains("dither_method="), "{filter}");
        assert!(!filter.contains("out_sample_fmt="), "{filter}");
    }

    #[test]
    fn ffmpeg_lossy_album_gain_pins_measured_encoder_input_rate() {
        let mut request = album_gain_pcm_request(AudioFormat::Aac);
        request.settings.target_format = AudioFormat::Aac;
        request.output_path = PathBuf::from("output.m4a");
        let step = PlanStep::new(
            0,
            PlanOperation::EncodeLossy {
                target_format: AudioFormat::Aac,
                target_rate_hz: Some(96_000),
                apply_processing: true,
            },
            InputSource::Path(PathBuf::from("album-carrier.f64le")),
            OutputSink::Path(PathBuf::from("output.m4a")),
            "AAC album gain application test",
        );

        let command = build_ffmpeg_encode_lossy(
            &request.context(),
            &step,
            &AudioFormat::Aac,
            Some(96_000),
            true,
        )
        .expect("supported AAC hard-ceiling encode command");
        assert_arg(&command.args, "-ar", "96000");
        assert!(
            command
                .args
                .iter()
                .any(|arg| arg == "volume=2.125000000dB:precision=double"),
            "album gain must precede a rate-pinned encoder input: {:?}",
            command.args,
        );
        assert!(
            !command.args.iter().any(|arg| arg.contains("aresample=")),
            "hard-ceiling carrier must not be resampled after the proved gain: {:?}",
            command.args,
        );

        // The gain may already have been realized by a preceding SoX PCM
        // processing step. The final FFmpeg encode still has to pin the same
        // measured rate even though it must not apply `volume` a second time.
        let post_gain_step = PlanStep::new(
            1,
            PlanOperation::EncodeLossy {
                target_format: AudioFormat::Aac,
                target_rate_hz: Some(96_000),
                apply_processing: false,
            },
            InputSource::Path(PathBuf::from("post-gain.wav")),
            OutputSink::Path(PathBuf::from("output.m4a")),
            "AAC post-gain encode rate pin test",
        );
        let post_gain_command = build_ffmpeg_encode_lossy(
            &request.context(),
            &post_gain_step,
            &AudioFormat::Aac,
            Some(96_000),
            false,
        )
        .expect("post-gain AAC encode must keep the measured rate pinned");
        assert_arg(&post_gain_command.args, "-ar", "96000");
        assert!(
            !post_gain_command
                .args
                .iter()
                .any(|arg| arg.starts_with("volume=")),
            "already-realized gain must not be applied twice: {:?}",
            post_gain_command.args,
        );

        let error = build_ffmpeg_encode_lossy(
            &request.context(),
            &step,
            &AudioFormat::Aac,
            None,
            true,
        )
        .expect_err("hard-ceiling lossy encode must fail closed without a pinned rate");
        assert!(error.to_string().contains("pin the encoder-input rate"), "{error}");
    }

    #[test]
    fn ffmpeg_lossy_album_gain_rejects_unsupported_measured_rate() {
        let mut request = album_gain_pcm_request(AudioFormat::Aac);
        request.settings.target_format = AudioFormat::Aac;
        request.output_path = PathBuf::from("output.m4a");
        request.source.sample_rate_hz = Some(192_000);
        let step = PlanStep::new(
            0,
            PlanOperation::EncodeLossy {
                target_format: AudioFormat::Aac,
                target_rate_hz: Some(192_000),
                apply_processing: true,
            },
            InputSource::Path(PathBuf::from("album-carrier.f64le")),
            OutputSink::Path(PathBuf::from("output.m4a")),
            "AAC unsupported-rate album gain test",
        );

        let error = build_ffmpeg_encode_lossy(
            &request.context(),
            &step,
            &AudioFormat::Aac,
            Some(192_000),
            true,
        )
        .expect_err("AAC 192 kHz must not fall through to FFmpeg auto-resampling");
        assert!(error.to_string().contains("refusing FFmpeg rate conversion"), "{error}");
    }

    #[test]
    fn hard_ceiling_lossless_dither_routes_processing_encode_to_sox() {
        let mut request = album_gain_pcm_request(AudioFormat::Flac);
        request.settings.dither_type = DitherType::Shibata;
        request.settings.dither_explicit = true;
        let step = album_gain_encode_step(AudioFormat::Flac, true);

        assert!(
            !FfmpegPlugin.supports(&request.context(), &step).is_supported(),
            "FFmpeg must not own a dithered hard-ceiling terminal stage",
        );
        assert!(
            SoxPlugin.supports(&request.context(), &step).is_supported(),
            "SoX must own the dithered hard-ceiling terminal stage",
        );
    }

    #[test]
    fn pcm_true_peak_lossless_dither_routes_processing_encode_to_sox() {
        let mut request = album_gain_pcm_request(AudioFormat::Flac);
        request.settings.dsd = crate::settings::DsdSettings::default();
        request.settings.pcm_true_peak.set_policy(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: crate::enums::TruePeakScope::Track,
            scan: crate::enums::TruePeakScanTier::Fast,
        });
        request.source.source_representation = crate::source::SourceRepresentationKind::Pcm;
        request.settings.dither_type = DitherType::Shibata;
        request.settings.dither_explicit = true;
        let step = album_gain_encode_step(AudioFormat::Flac, true);

        assert!(
            !FfmpegPlugin.supports(&request.context(), &step).is_supported(),
            "FFmpeg must not own a dithered PCM true-peak terminal stage",
        );
        assert!(
            SoxPlugin.supports(&request.context(), &step).is_supported(),
            "SoX must own the dithered PCM true-peak terminal stage",
        );
    }

    #[test]
    fn sox_pcm_album_gain_is_owned_only_by_processing_encode_step() {
        let request = album_gain_pcm_request(AudioFormat::Flac);
        for (apply_processing, expected_count) in [(false, 0), (true, 1)] {
            let step = album_gain_encode_step(AudioFormat::Flac, apply_processing);
            let command = build_sox_encode_pcm(
                &request.context(),
                &step,
                &AudioFormat::Flac,
                None,
                PcmBitDepth::Int24,
                apply_processing,
            )
            .expect("SoX album-gain encode command");
            let count = command
                .args
                .windows(2)
                .filter(|pair| pair[0] == "gain" && pair[1] == "2.125000000")
                .count();
            assert_eq!(count, expected_count, "{:?}", command.args);
        }
    }

    #[test]
    fn album_gain_carrier_binds_explicit_raw_f64le_input_contract() {
        let request = album_gain_pcm_request(AudioFormat::Flac);
        let step = album_gain_encode_step(AudioFormat::Flac, true);

        let ffmpeg = build_ffmpeg_encode_pcm(
            &request.context(),
            &step,
            &AudioFormat::Flac,
            None,
            PcmBitDepth::Int24,
            true,
        )
        .expect("FFmpeg raw album-gain carrier command");
        assert_arg(&ffmpeg.args, "-f", "f64le");
        assert_arg(&ffmpeg.args, "-ar", "96000");
        assert_arg(&ffmpeg.args, "-ac", "2");
        let ffmpeg_input = ffmpeg
            .args
            .windows(2)
            .find(|pair| pair[0] == "-i")
            .map(|pair| pair[1].as_str());
        assert_eq!(ffmpeg_input, Some("album-carrier.f64le"), "{:?}", ffmpeg.args);

        let sox = build_sox_encode_pcm(
            &request.context(),
            &step,
            &AudioFormat::Flac,
            None,
            PcmBitDepth::Int24,
            true,
        )
        .expect("SoX raw album-gain carrier command");
        assert_arg(&sox.args, "-t", "raw");
        assert_arg(&sox.args, "-e", "floating-point");
        assert_arg(&sox.args, "-b", "64");
        assert!(sox.args.iter().any(|arg| arg == "-L"), "{:?}", sox.args);
        assert_arg(&sox.args, "-r", "96000");
        assert_arg(&sox.args, "-c", "2");
        assert!(
            sox.args.iter().any(|arg| arg == "album-carrier.f64le"),
            "{:?}",
            sox.args
        );
    }

    #[test]
    fn explicit_int32_dither_is_emitted_by_ffmpeg_for_float_requantization() {
        let mut settings = PipelineSettings::default();
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        settings.dither_type = DitherType::Tpdf;
        settings.dither_explicit = true;
        let request = pcm_request_with(settings, PcmBitDepth::Float32);

        let filter = ffmpeg_audio_filter(
            &request.context(),
            None,
            Some(PcmBitDepth::Int32),
        )
        .expect("explicit Int32 FFmpeg filter");

        assert!(filter.contains("dither_method=triangular"), "{filter}");
        assert!(filter.contains("out_sample_fmt=s32"), "{filter}");
    }

    #[test]
    fn explicit_int32_terminal_contract_matches_production_ffmpeg_lowering() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        settings.dither_type = DitherType::Tpdf;
        settings.dither_explicit = true;
        let mut request = pcm_request_with(settings, PcmBitDepth::Float32);
        request.output_path = PathBuf::from("output.flac");

        let typed = match crate::semantic_plan::plan_typed(&request)
            .expect("typed terminal plan")
        {
            crate::semantic_plan::PlanningOutcome::Ready(plan) => plan,
            other => panic!("ordinary explicit Int32 terminal should be Ready: {other:?}"),
        };
        let realization = typed.nodes.iter().find_map(|node| match node {
            crate::semantic_plan::TypedPlanNode::Operation {
                operation: PlanOperation::EncodePcm { .. },
                candidates,
                selected_candidate,
                ..
            } => candidates.get(*selected_candidate).and_then(|candidate| {
                candidate.contract.terminal_realization.as_ref()
            }),
            _ => None,
        }).expect("selected terminal realization");
        let crate::semantic_plan::SelectedTerminalRealization::Pcm(realization) = realization else {
            panic!("expected PCM terminal realization")
        };
        assert_eq!(realization.effective_dither, Some(DitherType::Tpdf));

        let lowered = crate::plan_conversion(&request).expect("production command lowering");
        let PlanAction::Execute { commands, .. } = lowered.action else {
            panic!("depth-changing ordinary conversion must execute")
        };
        let dither_methods = commands
            .iter()
            .flat_map(|command| command.args.iter())
            .filter(|arg| arg.contains("dither_method="))
            .collect::<Vec<_>>();
        assert_eq!(dither_methods.len(), 1, "{dither_methods:?}");
        assert!(dither_methods[0].contains("dither_method=triangular"));
    }

    #[test]
    fn same_depth_explicit_int32_terminal_executes_ffmpeg_dither() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        settings.dither_type = DitherType::Tpdf;
        settings.dither_explicit = true;
        let mut request = pcm_request_with(settings, PcmBitDepth::Int32);
        request.output_path = PathBuf::from("output.flac");

        let lowered = crate::plan_conversion(&request)
            .expect("same-depth explicit Int32 FFmpeg dither must lower");
        let PlanAction::Execute { commands, .. } = lowered.action else {
            panic!("same-depth explicit Int32 dither must execute")
        };
        let dither_commands = commands
            .iter()
            .filter(|command| {
                command
                    .args
                    .iter()
                    .any(|arg| arg.contains("dither_method=triangular"))
            })
            .collect::<Vec<_>>();
        assert_eq!(dither_commands.len(), 1, "{commands:#?}");
        assert_eq!(dither_commands[0].tool, ToolIdentifier::Ffmpeg);
        let dither_method_count = commands
            .iter()
            .flat_map(|command| command.args.iter())
            .map(|arg| arg.matches("dither_method=").count())
            .sum::<usize>();
        assert_eq!(dither_method_count, 1, "{commands:#?}");

        let mut control = request.clone();
        control.settings.dither_explicit = false;
        let lowered = crate::plan_conversion(&control)
            .expect("same-depth non-explicit Int32 control must lower");
        let PlanAction::Execute { commands, .. } = lowered.action else {
            panic!("format-changing control must execute")
        };
        assert!(
            !commands
                .iter()
                .flat_map(|command| command.args.iter())
                .any(|arg| arg.contains("dither_method=")),
            "non-explicit Int32 control must remain undithered: {commands:#?}",
        );
    }

    #[test]
    fn auto_backend_realizes_explicit_int32_dither_with_ffmpeg_for_wav() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Wav;
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        settings.dither_type = DitherType::Tpdf;
        settings.dither_explicit = true;
        settings.metadata.preserve_artwork = false;
        let request = pcm_request_with(settings, PcmBitDepth::Float64);

        let typed = match crate::semantic_plan::plan_typed(&request)
            .expect("typed explicit Int32 WAV plan")
        {
            crate::semantic_plan::PlanningOutcome::Ready(plan) => plan,
            other => panic!("ordinary explicit Int32 WAV terminal should be Ready: {other:?}"),
        };
        let realization = typed
            .nodes
            .iter()
            .find_map(|node| match node {
                crate::semantic_plan::TypedPlanNode::Operation {
                    operation: PlanOperation::EncodePcm { .. },
                    candidates,
                    selected_candidate,
                    ..
                } => candidates
                    .get(*selected_candidate)
                    .and_then(|candidate| candidate.contract.terminal_realization.as_ref()),
                _ => None,
            })
            .expect("selected terminal realization");
        let crate::semantic_plan::SelectedTerminalRealization::Pcm(realization) = realization else {
            panic!("expected PCM terminal realization")
        };
        assert_eq!(realization.selected_tool, ToolIdentifier::Ffmpeg);
        assert_eq!(realization.effective_dither, Some(DitherType::Tpdf));

        let lowered = crate::plan_conversion(&request)
            .expect("Auto must realize the selected FFmpeg Int32 terminal");
        let PlanAction::Execute { commands, .. } = lowered.action else {
            panic!("Float64 to WAV Int32 must execute")
        };
        let terminal = commands
            .iter()
            .find(|command| {
                command
                    .args
                    .iter()
                    .any(|arg| arg.contains("dither_method=triangular"))
            })
            .expect("physical FFmpeg dither terminal");
        assert_eq!(terminal.tool, ToolIdentifier::Ffmpeg, "{commands:#?}");
        assert!(
            commands.iter().all(|command| command.tool != ToolIdentifier::Sox),
            "SoX must not replace the selected explicit Int32 FFmpeg terminal: {commands:#?}",
        );

        let mut control = request.clone();
        control.settings.dither_explicit = false;
        let lowered = crate::plan_conversion(&control)
            .expect("non-explicit Int32 WAV control must lower");
        let PlanAction::Execute { commands, .. } = lowered.action else {
            panic!("Float64 to WAV Int32 control must execute")
        };
        assert!(
            commands.iter().all(|command| {
                !command.args.iter().any(|arg| {
                    arg == "dither" || arg.contains("dither_method=")
                })
            }),
            "non-explicit Int32 control must not gain dither: {commands:#?}",
        );
    }

    #[test]
    fn same_format_source_depth_explicit_int32_dither_blocks_passthrough() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Wav;
        settings.target_bit_depth = BitDepthTarget::Source;
        settings.dither_type = DitherType::Tpdf;
        settings.dither_explicit = true;
        settings.metadata.preserve_artwork = false;
        let request = pcm_request_with(settings, PcmBitDepth::Int32);

        let lowered = crate::plan_conversion(&request)
            .expect("same-format explicit Int32 dither must lower");
        let PlanAction::Execute { commands, .. } = lowered.action else {
            panic!("explicit Int32 dither must block passthrough")
        };
        let terminal = commands
            .iter()
            .find(|command| {
                command
                    .args
                    .iter()
                    .any(|arg| arg.contains("dither_method=triangular"))
            })
            .expect("explicit same-format FFmpeg dither terminal");
        assert_eq!(terminal.tool, ToolIdentifier::Ffmpeg, "{commands:#?}");

        let mut control = request.clone();
        control.settings.dither_explicit = false;
        control.settings.metadata.preserve_artwork = true;
        let lowered = crate::plan_conversion(&control)
            .expect("same-format non-explicit Int32 control must plan");
        assert!(
            matches!(&lowered.action, PlanAction::PassthroughCopy { .. }),
            "non-explicit same-format Int32 control should retain passthrough eligibility: {lowered:#?}",
        );
    }

    #[test]
    fn ffmpeg_int32_explicitness_uses_settings_depth_when_step_depth_is_absent() {
        let mut settings = PipelineSettings::default();
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        settings.dither_type = DitherType::Tpdf;
        settings.dither_explicit = true;
        let request = pcm_request_with(settings, PcmBitDepth::Float32);

        let filter = ffmpeg_audio_filter(&request.context(), None, None)
            .expect("depth-carried-elsewhere FFmpeg filter");

        assert!(filter.contains("dither_method=triangular"), "{filter}");
    }

    #[test]
    fn automatic_int32_dither_stays_disabled_for_ffmpeg_and_sox() {
        let mut settings = PipelineSettings::default();
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        settings.dither_type = DitherType::Tpdf;
        let request = pcm_request_with(settings, PcmBitDepth::Float32);

        let filter = ffmpeg_audio_filter(
            &request.context(),
            None,
            Some(PcmBitDepth::Int32),
        )
        .expect("automatic Int32 FFmpeg filter");
        assert!(!filter.contains("dither_method="), "{filter}");
        assert!(filter.contains("out_sample_fmt=s32"), "{filter}");

        let mut sox_args = Vec::new();
        add_sox_pcm_effects(
            &request.context(),
            &mut sox_args,
            None,
            Some(PcmBitDepth::Int32),
        );
        assert!(!sox_args.iter().any(|arg| arg == "dither"), "{sox_args:?}");

        let mut depth_carried_elsewhere_args = Vec::new();
        add_sox_pcm_effects(
            &request.context(),
            &mut depth_carried_elsewhere_args,
            None,
            None,
        );
        assert!(
            !depth_carried_elsewhere_args.iter().any(|arg| arg == "dither"),
            "{depth_carried_elsewhere_args:?}"
        );
    }

    #[test]
    fn wavpack_hybrid_float_source_int32_tpdf_predicate_is_narrow_and_accepts_explicit_tpdf() {
        for source_depth in [PcmBitDepth::Float32, PcmBitDepth::Float64] {
            let mut settings = PipelineSettings::default();
            settings.target_format = AudioFormat::WavPack;
            settings.wavpack.hybrid = true;
            settings.target_bit_depth = BitDepthTarget::Source;
            settings.dither_type = DitherType::Tpdf;
            settings.dither_explicit = true;
            let request = pcm_request_with(settings, source_depth);
            assert!(
                wavpack_hybrid_float_source_int32_tpdf_requested(
                    &request,
                    Some(PcmBitDepth::Int32),
                ),
                "{source_depth:?}",
            );
        }

        let mut explicit_depth = PipelineSettings::default();
        explicit_depth.target_format = AudioFormat::WavPack;
        explicit_depth.wavpack.hybrid = true;
        explicit_depth.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        explicit_depth.dither_type = DitherType::Tpdf;
        explicit_depth.dither_explicit = true;
        let explicit_depth = pcm_request_with(explicit_depth, PcmBitDepth::Float32);
        assert!(!wavpack_hybrid_float_source_int32_tpdf_requested(
            &explicit_depth,
            Some(PcmBitDepth::Int32),
        ));

        let mut wrong_dither = PipelineSettings::default();
        wrong_dither.target_format = AudioFormat::WavPack;
        wrong_dither.wavpack.hybrid = true;
        wrong_dither.target_bit_depth = BitDepthTarget::Source;
        wrong_dither.dither_type = DitherType::SlopedTpdf;
        wrong_dither.dither_explicit = true;
        let wrong_dither = pcm_request_with(wrong_dither, PcmBitDepth::Float32);
        assert!(!wavpack_hybrid_float_source_int32_tpdf_requested(
            &wrong_dither,
            Some(PcmBitDepth::Int32),
        ));

        let mut integer_source = PipelineSettings::default();
        integer_source.target_format = AudioFormat::WavPack;
        integer_source.wavpack.hybrid = true;
        integer_source.target_bit_depth = BitDepthTarget::Source;
        integer_source.dither_type = DitherType::Tpdf;
        integer_source.dither_explicit = true;
        let integer_source = pcm_request_with(integer_source, PcmBitDepth::Int24);
        assert!(!wavpack_hybrid_float_source_int32_tpdf_requested(
            &integer_source,
            Some(PcmBitDepth::Int32),
        ));
    }

    #[test]
    fn explicit_int32_dither_is_refused_by_unqualified_sox_pcm_builders() {
        let mut settings = PipelineSettings::default();
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        settings.dither_type = DitherType::Tpdf;
        settings.dither_explicit = true;
        let request = pcm_request_with(settings, PcmBitDepth::Int24);

        for step_depth in [Some(PcmBitDepth::Int32), None] {
            let mut args = Vec::new();
            add_sox_pcm_effects(
                &request.context(),
                &mut args,
                None,
                step_depth,
            );
            assert!(
                !args.iter().any(|arg| arg == "dither"),
                "ordinary SoX Int32 dither must remain fail-closed: {args:?}"
            );
        }

        let mut source_settings = PipelineSettings::default();
        source_settings.target_bit_depth = BitDepthTarget::Source;
        source_settings.dither_type = DitherType::Tpdf;
        source_settings.dither_explicit = true;
        let source_preserved = pcm_request_with(source_settings, PcmBitDepth::Int32);
        let mut source_preserved_args = Vec::new();
        add_sox_pcm_effects(
            &source_preserved.context(),
            &mut source_preserved_args,
            None,
            None,
        );
        assert!(
            !source_preserved_args.iter().any(|arg| arg == "dither"),
            "source-preserved Int32 SoX dither must remain fail-closed: {source_preserved_args:?}"
        );

        let mut dsd_to_pcm_args = Vec::new();
        add_sox_dsd_to_pcm_effects(
            &request.context(),
            &mut dsd_to_pcm_args,
            88_200,
            PcmBitDepth::Int32,
            DsdLowpassMethod::Auto,
        )
        .expect("explicit Int32 SoX DSD-to-PCM effects");
        assert!(
            !dsd_to_pcm_args.iter().any(|arg| arg == "dither"),
            "ordinary SoX DSD-to-PCM Int32 dither must remain fail-closed: {dsd_to_pcm_args:?}"
        );
    }

    #[test]
    fn explicit_gesemann_int32_ffmpeg_fails_closed_instead_of_dropping_dither() {
        let mut settings = PipelineSettings::default();
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        settings.dither_type = DitherType::Gesemann;
        settings.dither_explicit = true;
        let request = pcm_request_with(settings, PcmBitDepth::Float32);

        let err = ffmpeg_audio_filter(
            &request.context(),
            None,
            Some(PcmBitDepth::Int32),
        )
        .expect_err("unsupported explicit Int32 dither must fail closed");

        match err {
            PlanningError::InvalidSettings { field, reason } => {
                assert_eq!(field, "dither_type");
                assert!(reason.contains("not supported by FFmpeg/SoXR"), "{reason}");
            }
            other => panic!("expected InvalidSettings, got {other:?}"),
        }
    }

    #[test]
    fn explicit_dither_is_never_emitted_for_float_targets() {
        let mut settings = PipelineSettings::default();
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Float32);
        settings.dither_type = DitherType::Tpdf;
        settings.dither_explicit = true;
        let request = pcm_request_with(settings, PcmBitDepth::Int24);

        let filter = ffmpeg_audio_filter(
            &request.context(),
            None,
            Some(PcmBitDepth::Float32),
        )
        .expect("float FFmpeg filter");
        assert!(!filter.contains("dither_method="), "{filter}");

        let mut sox_args = Vec::new();
        add_sox_pcm_effects(
            &request.context(),
            &mut sox_args,
            None,
            Some(PcmBitDepth::Float32),
        );
        assert!(!sox_args.iter().any(|arg| arg == "dither"), "{sox_args:?}");
    }

    #[test]
    fn explicit_int32_dither_direct_ssrc_terminal_remains_uncommissioned() {
        let mut settings = PipelineSettings::default();
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        settings.dither_type = DitherType::Tpdf;
        settings.dither_explicit = true;

        let error = ssrc_resample_command_with(
            settings.clone(),
            44_100,
            Some(PcmBitDepth::Int32),
        )
        .expect_err("direct SSRC Int32 dither ownership is not commissioned");
        assert!(format!("{error:?}").contains("dither mapping is unavailable"));

        let error = ssrc_resample_command_with(settings, 44_100, None)
            .expect_err("settings-carried Int32 dither must remain uncommissioned");
        assert!(format!("{error:?}").contains("dither mapping is unavailable"));
    }

    #[test]
    fn ssrc_rate_change_keeps_explicit_int32_dither_on_final_ffmpeg_terminal() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.target_sample_rate = crate::enums::RateTarget::PcmHz(44_100);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        settings.nyquist_transition = crate::enums::NyquistTransition::BrickWall;
        settings.dither_type = DitherType::Tpdf;
        settings.dither_explicit = true;
        let mut request = pcm_request_with(settings, PcmBitDepth::Int32);
        request.output_path = PathBuf::from("output.flac");

        let typed = match crate::semantic_plan::plan_typed(&request)
            .expect("typed SSRC explicit Int32 plan")
        {
            crate::semantic_plan::PlanningOutcome::Ready(plan) => plan,
            other => panic!("ordinary SSRC explicit Int32 route should be Ready: {other:?}"),
        };
        let realization = typed
            .nodes
            .iter()
            .find_map(|node| match node {
                crate::semantic_plan::TypedPlanNode::Operation {
                    operation: PlanOperation::EncodePcm { .. },
                    candidates,
                    selected_candidate,
                    ..
                } => candidates
                    .get(*selected_candidate)
                    .and_then(|candidate| candidate.contract.terminal_realization.as_ref()),
                _ => None,
            })
            .expect("selected terminal realization");
        let crate::semantic_plan::SelectedTerminalRealization::Pcm(realization) = realization else {
            panic!("expected PCM terminal realization")
        };
        assert_eq!(realization.selected_tool, ToolIdentifier::Ffmpeg);
        assert_eq!(
            realization.input_precision,
            crate::semantic_plan::StoragePrecision::Pcm(PcmBitDepth::Float64)
        );
        assert_eq!(
            realization.input_value_domain,
            crate::semantic_plan::ValueDomain::FiniteFloating
        );
        assert_eq!(realization.effective_dither, Some(DitherType::Tpdf));

        let lowered = crate::plan_conversion(&request)
            .expect("SSRC route must preserve the selected final FFmpeg Int32 dither");
        let PlanAction::Execute { commands, .. } = lowered.action else {
            panic!("rate-changing SSRC route must execute")
        };

        let ssrc = commands
            .iter()
            .find(|command| command.tool == ToolIdentifier::Ssrc)
            .expect("SSRC resample command");
        assert_arg(&ssrc.args, "--bits", "-64");
        assert_no_arg(&ssrc.args, "--dither");
        assert_no_arg(&ssrc.args, "--pdf");

        let dither_commands = commands
            .iter()
            .filter(|command| {
                command
                    .args
                    .iter()
                    .any(|arg| arg.contains("dither_method=triangular"))
            })
            .collect::<Vec<_>>();
        assert_eq!(dither_commands.len(), 1, "{commands:#?}");
        assert_eq!(dither_commands[0].tool, ToolIdentifier::Ffmpeg);
        let dither_method_count = commands
            .iter()
            .flat_map(|command| command.args.iter())
            .map(|arg| arg.matches("dither_method=").count())
            .sum::<usize>();
        assert_eq!(dither_method_count, 1, "{commands:#?}");

        let mut control = request.clone();
        control.settings.dither_explicit = false;
        let lowered = crate::plan_conversion(&control)
            .expect("SSRC non-explicit Int32 control must lower");
        let PlanAction::Execute { commands, .. } = lowered.action else {
            panic!("rate-changing SSRC control must execute")
        };
        let ssrc = commands
            .iter()
            .find(|command| command.tool == ToolIdentifier::Ssrc)
            .expect("SSRC resample command");
        assert_arg(&ssrc.args, "--bits", "-64");
        assert_no_arg(&ssrc.args, "--dither");
        assert_no_arg(&ssrc.args, "--pdf");
        assert!(
            !commands
                .iter()
                .flat_map(|command| command.args.iter())
                .any(|arg| arg.contains("dither_method=")),
            "non-explicit Int32 control must leave final FFmpeg packaging undithered: {commands:#?}",
        );
    }

    #[test]
    fn ssrc_command_emits_global_tpdf_as_no_shaper_with_triangular_pdf() {
        let mut settings = PipelineSettings::default();
        settings.dither_type = DitherType::Tpdf;

        let command = ssrc_resample_command_with(settings, 44_100, Some(PcmBitDepth::Int16)).unwrap();

        assert_arg(&command.args, "--dither", "99");
        assert_arg(&command.args, "--pdf", "1");
        assert_arg(&command.args, "--bits", "16");
    }

    #[test]
    fn ssrc_command_emits_global_none_as_no_dither_or_pdf() {
        let mut settings = PipelineSettings::default();
        settings.dither_type = DitherType::None;

        let command = ssrc_resample_command_with(settings, 44_100, Some(PcmBitDepth::Int16)).unwrap();

        assert_no_arg(&command.args, "--dither");
        assert_no_arg(&command.args, "--pdf");
    }

    #[test]
    fn same_depth_int16_tpdf_is_still_owned_by_the_ssrc_integer_terminal() {
        let mut settings = PipelineSettings::default();
        settings.dither_type = DitherType::Tpdf;

        let resolved = resolve_ssrc_dither_for_rate(
            &settings,
            Some(PcmBitDepth::Int16),
            Some(PcmBitDepth::Int16),
            44_100,
        )
        .expect("same-depth terminal dither resolution");

        assert_eq!(resolved.dither_id, Some(99));
        assert_eq!(resolved.pdf_type, Some(SsrcPdfType::Triangular));
        assert_eq!(resolved.origin, SsrcDitherOrigin::GlobalExact);
        assert_eq!(resolved.availability, SsrcDitherAvailability::Active);
    }

    #[test]
    fn pdf_only_native_override_with_global_none_defaults_to_id99_with_explicit_pdf() {
        let mut settings = PipelineSettings::default();
        settings.dither_type = DitherType::None;
        settings.ssrc.pdf_type = Some(SsrcPdfType::Rectangular);

        let resolved = resolve_ssrc_dither_for_rate(
            &settings,
            Some(PcmBitDepth::Int24),
            Some(PcmBitDepth::Int16),
            44_100,
        )
        .expect("PDF-only native override");

        assert_eq!(resolved.dither_id, Some(99));
        assert_eq!(resolved.pdf_type, Some(SsrcPdfType::Rectangular));
        assert_eq!(resolved.origin, SsrcDitherOrigin::NativeOverride);
        assert_eq!(resolved.availability, SsrcDitherAvailability::Active);
    }

    #[test]
    fn ssrc_command_emits_shibata_family_as_rate_valid_ath_curve_a_with_triangular_pdf() {
        let cases = [
            (DitherType::LowShibata, 44_100, "0"),
            (DitherType::Shibata, 44_100, "2"),
            (DitherType::HighShibata, 44_100, "6"),
            (DitherType::HighShibata, 96_000, "2"),
            (DitherType::HighShibata, 22_050, "1"),
        ];

        for (dither_type, target_rate_hz, expected_dither_id) in cases {
            let mut settings = PipelineSettings::default();
            settings.dither_type = dither_type;

            let command = ssrc_resample_command_with(
                settings,
                target_rate_hz,
                Some(PcmBitDepth::Int16),
            )
            .unwrap();

            assert_arg(&command.args, "--dither", expected_dither_id);
            assert_arg(&command.args, "--pdf", "1");
        }
    }

    #[test]
    fn ssrc_command_honors_explicit_dither_id_only() {
        let mut settings = PipelineSettings::default();
        settings.dither_type = DitherType::Tpdf;
        settings.ssrc.dither_id = Some(0);
        settings.ssrc.pdf_type = None;

        let command = ssrc_resample_command_with(settings, 44_100, Some(PcmBitDepth::Int16)).unwrap();

        assert_arg(&command.args, "--dither", "0");
        assert_arg(&command.args, "--pdf", "1");
    }

    #[test]
    fn ssrc_command_honors_explicit_pdf_type_only() {
        let mut settings = PipelineSettings::default();
        settings.dither_type = DitherType::Shibata;
        settings.ssrc.dither_id = None;
        settings.ssrc.pdf_type = Some(SsrcPdfType::Rectangular);

        let command = ssrc_resample_command_with(settings, 44_100, Some(PcmBitDepth::Int16)).unwrap();

        assert_arg(&command.args, "--dither", "2");
        assert_arg(&command.args, "--pdf", "0");
    }

    #[test]
    fn ssrc_command_honors_both_explicit_overrides() {
        let mut settings = PipelineSettings::default();
        settings.dither_type = DitherType::None;
        settings.ssrc.dither_id = Some(1);
        settings.ssrc.pdf_type = Some(SsrcPdfType::Triangular);

        let command = ssrc_resample_command_with(settings, 22_050, Some(PcmBitDepth::Int16)).unwrap();

        assert_arg(&command.args, "--dither", "1");
        assert_arg(&command.args, "--pdf", "1");
    }

    #[test]
    fn ssrc_command_suppresses_dither_and_pdf_for_float_output() {
        let mut settings = PipelineSettings::default();
        settings.dither_type = DitherType::Shibata;
        settings.ssrc.dither_id = Some(2);
        settings.ssrc.pdf_type = Some(SsrcPdfType::Triangular);

        let command = ssrc_resample_command_with(settings, 44_100, Some(PcmBitDepth::Float32)).unwrap();

        assert_arg(&command.args, "--bits", "-32");
        assert_no_arg(&command.args, "--dither");
        assert_no_arg(&command.args, "--pdf");
    }

    #[test]
    fn ssrc_command_rejects_explicit_high_rate_unavailable_dither_id() {
        let mut settings = PipelineSettings::default();
        settings.ssrc.dither_id = Some(16);

        let err = ssrc_resample_command_with(settings, 96_000, Some(PcmBitDepth::Int16)).unwrap_err();

        let message = format!("{err:?}");
        assert!(message.contains("96"));
        assert!(message.contains("16"));
    }

    #[test]
    fn ssrc_command_rejects_explicit_low_rate_unavailable_dither_id() {
        let mut settings = PipelineSettings::default();
        settings.ssrc.dither_id = Some(6);

        let err = ssrc_resample_command_with(settings, 22_050, Some(PcmBitDepth::Int16)).unwrap_err();

        let message = format!("{err:?}");
        assert!(message.contains("22050") || message.contains("22_050"));
        assert!(message.contains("6"));
    }


    #[test]
    fn ssrc_command_rejects_derived_global_shaper_at_unlisted_rate() {
        let mut settings = PipelineSettings::default();
        settings.dither_type = DitherType::HighShibata;

        let err = ssrc_resample_command_with(settings, 176_400, Some(PcmBitDepth::Int16)).unwrap_err();

        let message = format!("{err:?}");
        assert!(message.contains("176400") || message.contains("176_400"));
    }

    #[test]
    fn planner_format_metadata_capabilities_are_centralized_on_audio_format() {
        assert!(AudioFormat::Flac.supports_planner_source_tag_transfer());
        assert!(AudioFormat::Wav.supports_planner_source_tag_transfer());
        assert!(AudioFormat::Mp3.supports_planner_source_tag_transfer());
        assert!(!AudioFormat::Opus.supports_planner_source_tag_transfer());
        assert!(!AudioFormat::Dsf.supports_planner_source_tag_transfer());
        assert!(!AudioFormat::Dff.supports_planner_source_tag_transfer());

        assert!(AudioFormat::Flac.supports_planner_embedded_artwork_transfer());
        assert!(AudioFormat::Mp3.supports_planner_embedded_artwork_transfer());
        assert!(AudioFormat::Aac.supports_planner_embedded_artwork_transfer());
        assert!(AudioFormat::Alac.supports_planner_embedded_artwork_transfer());
        assert!(!AudioFormat::Opus.supports_planner_embedded_artwork_transfer());
        assert!(!AudioFormat::Wav.supports_planner_embedded_artwork_transfer());
        assert!(!AudioFormat::Dsf.supports_planner_embedded_artwork_transfer());

        assert!(AudioFormat::Flac.supports_cue_post_encode_artwork_embedding());
        assert!(AudioFormat::Mp3.supports_cue_post_encode_artwork_embedding());
        assert!(AudioFormat::Aac.supports_cue_post_encode_artwork_embedding());
        assert!(AudioFormat::Alac.supports_cue_post_encode_artwork_embedding());
        assert!(AudioFormat::WavPack.supports_cue_post_encode_artwork_embedding());
        assert!(!AudioFormat::Opus.supports_cue_post_encode_artwork_embedding());
        assert!(!AudioFormat::Wav.supports_cue_post_encode_artwork_embedding());
        assert!(!AudioFormat::Aiff.supports_cue_post_encode_artwork_embedding());
        assert!(!AudioFormat::Dsf.supports_cue_post_encode_artwork_embedding());
    }

    #[test]
    fn wavpack_float32_command_forces_ffmpeg_float_sample_format() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::WavPack;
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Float32);
        settings.metadata.preserve_artwork = false;
        let mut request = pcm_request_with(settings, PcmBitDepth::Int24);
        request.output_path = PathBuf::from("output.wv");

        let plan = crate::plan_conversion(&request)
            .expect("Int24 PCM -> lossless WavPack Float32 must plan");
        let PlanAction::Execute { commands, .. } = plan.action else {
            panic!("explicit Float32 conversion must execute");
        };
        let terminal = commands.last().expect("WavPack terminal command");
        assert_eq!(terminal.tool, ToolIdentifier::Ffmpeg);
        assert!(
            terminal
                .args
                .windows(2)
                .any(|pair| pair[0] == "-c:a" && pair[1] == "wavpack"),
            "{:?}",
            terminal.args,
        );
        let filter = arg_value(&terminal.args, "-af").expect("Float32 conversion filter");
        assert!(filter.contains("out_sample_fmt=flt"), "{filter}");
    }

    #[test]
    fn ffmpeg_is_unsupported_for_wavpack_int24_but_sox_still_encodes_it() {
        // ffmpeg's wavpack encoder cannot write 24-bit (it stores true
        // 32-bit ints) — the plan must never route this cell to ffmpeg.
        let source = SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Flac,
            codec: crate::enums::AudioCodec::Flac,
            sample_rate_hz: Some(44_100),
            bit_depth: Some(PcmBitDepth::Int24),
            true_source_depth: Some(PcmBitDepth::Int24),
            source_representation: Default::default(),
            sample_kind: Some(crate::enums::SampleKind::SignedInteger),
            channels: Some(2),
            duration: None,
            frame_extent: None,
            audio_md5: None,
        };
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::WavPack;
        settings.target_bit_depth = crate::enums::BitDepthTarget::Pcm(PcmBitDepth::Int24);
        let request = PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,

            input_path: PathBuf::from("realized.flac"),
            output_path: PathBuf::from("track.wv"),
            source,
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        };
        let step = |apply_processing: bool| {
            PlanStep::new(
                0,
                PlanOperation::EncodePcm {
                    target_format: AudioFormat::WavPack,
                    target_rate_hz: None,
                    target_bit_depth: PcmBitDepth::Int24,
                    apply_processing,
                },
                InputSource::Path(PathBuf::from("realized.flac")),
                OutputSink::Path(PathBuf::from("track.wv")),
                "Encode WavPack output",
            )
        };

        for apply_processing in [false, true] {
            let support = FfmpegPlugin.supports(&request.context(), &step(apply_processing));
            assert!(
                !support.is_supported(),
                "ffmpeg must be unsupported for WavPack Int24 (apply_processing={apply_processing})"
            );
            let sox = SoxPlugin.supports(&request.context(), &step(apply_processing));
            assert!(
                sox.is_supported(),
                "sox must remain available for WavPack Int24 (apply_processing={apply_processing})"
            );
        }

        // Faithful FFmpeg cells stay eligible. Float32 is the native
        // floating-point WavPack representation; Float64 is rejected earlier.
        for depth in [
            PcmBitDepth::Int16,
            PcmBitDepth::Int32,
            PcmBitDepth::Float32,
        ] {
            let step = PlanStep::new(
                0,
                PlanOperation::EncodePcm {
                    target_format: AudioFormat::WavPack,
                    target_rate_hz: None,
                    target_bit_depth: depth,
                    apply_processing: false,
                },
                InputSource::Path(PathBuf::from("realized.flac")),
                OutputSink::Path(PathBuf::from("track.wv")),
                "Encode WavPack output",
            );
            assert!(
                FfmpegPlugin.supports(&request.context(), &step).is_supported(),
                "ffmpeg should stay eligible for WavPack {depth:?}"
            );
            if depth == PcmBitDepth::Float32 {
                assert!(
                    !SoxPlugin.supports(&request.context(), &step).is_supported(),
                    "SoX must not own WavPack Float32 because it substitutes integer storage"
                );
            }
        }

        // Hybrid mode is exempt: the ffmpeg plugin delegates hybrid encodes
        // to the native wavpack CLI, which writes true 24-bit.
        let mut hybrid_settings = PipelineSettings::default();
        hybrid_settings.target_format = AudioFormat::WavPack;
        hybrid_settings.target_bit_depth =
            crate::enums::BitDepthTarget::Pcm(PcmBitDepth::Int24);
        hybrid_settings.wavpack.hybrid = true;
        hybrid_settings.metadata.preserve_artwork = false;
        let hybrid_request = PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,

            input_path: PathBuf::from("realized.flac"),
            output_path: PathBuf::from("track.wv"),
            source: request.source.clone(),
            settings: hybrid_settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        };
        assert!(
            FfmpegPlugin
                .supports(&hybrid_request.context(), &step(false))
                .is_supported(),
            "hybrid WavPack Int24 keeps the ffmpeg plugin (wavpack CLI delegate)"
        );
        let hybrid_plan = crate::plan_conversion(&hybrid_request)
            .expect("ordinary WavPack hybrid plan must preserve the native delegate");
        let PlanAction::Execute { commands, .. } = hybrid_plan.action else {
            panic!("ordinary WavPack hybrid conversion must execute")
        };
        assert!(
            commands
                .iter()
                .any(|command| command.tool == ToolIdentifier::Custom("wavpack".to_owned())),
            "selected terminal realization must admit the existing native wavpack delegate",
        );

        let mut hard_ceiling_hybrid = hybrid_request.clone();
        hard_ceiling_hybrid.settings.pcm_true_peak.set_policy(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: crate::enums::TruePeakScope::Track,
            scan: crate::enums::TruePeakScanTier::Fast,
        });
        assert!(
            !SoxPlugin
                .supports(&hard_ceiling_hybrid.context(), &step(false))
                .is_supported(),
            "PCM true-peak hybrid final encode must not bypass native wavpack -b via SoX",
        );
        assert!(
            FfmpegPlugin
                .supports(&hard_ceiling_hybrid.context(), &step(false))
                .is_supported(),
            "PCM true-peak hybrid final encode keeps the native wavpack delegate",
        );

        let mut fixed_gain_hybrid = hybrid_request.clone();
        fixed_gain_hybrid.settings.pcm_true_peak.set_policy(SampleGainPolicy::FixedGain {
            gain_db: "2.500000000".parse().expect("valid fixed gain"),
        });
        assert!(
            !SoxPlugin
                .supports(&fixed_gain_hybrid.context(), &step(false))
                .is_supported(),
            "PCM fixed-gain hybrid final encode must not bypass native wavpack -b via SoX",
        );
        assert!(
            FfmpegPlugin
                .supports(&fixed_gain_hybrid.context(), &step(false))
                .is_supported(),
            "PCM fixed-gain hybrid final encode keeps the native wavpack delegate",
        );

        hard_ceiling_hybrid.settings.dither_type = DitherType::Tpdf;
        hard_ceiling_hybrid.settings.dither_explicit = true;
        let integer_terminal_step = PlanStep::new(
            0,
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Wav,
                target_rate_hz: None,
                target_bit_depth: PcmBitDepth::Int24,
                apply_processing: true,
            },
            InputSource::Path(PathBuf::from("gained-carrier.f64le")),
            OutputSink::Path(PathBuf::from("encoder-input.wav")),
            "Realize WavPack hybrid encoder input",
        );
        assert!(
            !FfmpegPlugin
                .supports(&hard_ceiling_hybrid.context(), &integer_terminal_step)
                .is_supported(),
            "dithered hybrid integer-terminal realization must not use FFmpeg",
        );
        assert!(
            SoxPlugin
                .supports(&hard_ceiling_hybrid.context(), &integer_terminal_step)
                .is_supported(),
            "dithered hybrid integer-terminal realization must use the bounded SoX path",
        );

        let hybrid_step = PlanStep::new(
            0,
            PlanOperation::EncodePcm {
                target_format: AudioFormat::WavPack,
                target_rate_hz: None,
                target_bit_depth: PcmBitDepth::Int24,
                apply_processing: false,
            },
            InputSource::Path(PathBuf::from("encoder-input.wav")),
            OutputSink::Path(PathBuf::from("track.wv")),
            "Encode WavPack hybrid output",
        );
        let command = FfmpegPlugin
            .build_command(&hard_ceiling_hybrid.context(), &hybrid_step)
            .expect("hard-ceiling hybrid command");
        assert_eq!(command.tool, ToolIdentifier::Custom("wavpack".to_string()));
        assert!(command.args.iter().any(|arg| arg == "-b"));
        assert!(
            command.args.iter().any(|arg| arg == "-c"),
            "existing .wvc correction-file behavior must remain enabled",
        );
        assert!(command.args.iter().any(|arg| arg == "encoder-input.wav"));
    }

    #[test]
    fn ffmpeg_aac_command_rejects_raw_aac_suffix_without_raw_mode() {
        let source = SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Wav,
            codec: crate::enums::AudioCodec::PcmSigned,
            sample_rate_hz: Some(44_100),
            bit_depth: Some(PcmBitDepth::Int16),
            true_source_depth: Some(PcmBitDepth::Int16),
            source_representation: Default::default(),
            sample_kind: Some(crate::enums::SampleKind::SignedInteger),
            channels: Some(2),
            duration: None,
            frame_extent: None,
            audio_md5: None,
        };
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Aac;
        let request = PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,

            input_path: PathBuf::from("realized.wav"),
            output_path: PathBuf::from("track.aac"),
            source,
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        };
        let step = PlanStep::new(
            0,
            PlanOperation::EncodeLossy {
                target_format: AudioFormat::Aac,
                target_rate_hz: None,
                apply_processing: false,
            },
            InputSource::Path(PathBuf::from("realized.wav")),
            OutputSink::Path(PathBuf::from("track.aac")),
            "Encode AAC output",
        );

        let err = FfmpegPlugin
            .build_command(&request.context(), &step)
            .expect_err("raw .aac suffix must not produce an MP4 command");
        assert!(
            err.to_string().contains("raw .aac output is not implemented"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn ffmpeg_aac_m4b_command_uses_ipod_muxer() {
        let source = SourceInfo {
            dsd_source_kind: None,
            format: AudioFormat::Wav,
            codec: crate::enums::AudioCodec::PcmSigned,
            sample_rate_hz: Some(44_100),
            bit_depth: Some(PcmBitDepth::Int16),
            true_source_depth: Some(PcmBitDepth::Int16),
            source_representation: Default::default(),
            sample_kind: Some(crate::enums::SampleKind::SignedInteger),
            channels: Some(2),
            duration: None,
            frame_extent: None,
            audio_md5: None,
        };
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Aac;
        let request = PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,
            input_path: PathBuf::from("realized.wav"),
            output_path: PathBuf::from("track.m4b"),
            source,
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        };
        let step = PlanStep::new(
            0,
            PlanOperation::EncodeLossy {
                target_format: AudioFormat::Aac,
                target_rate_hz: Some(44_100),
                apply_processing: false,
            },
            InputSource::Path(PathBuf::from("realized.wav")),
            OutputSink::Path(PathBuf::from("track.m4b")),
            "Encode AAC M4B output",
        );

        let command = FfmpegPlugin
            .build_command(&request.context(), &step)
            .expect("AAC M4B command");
        assert!(command.args.windows(2).any(|window| window[0] == "-f" && window[1] == "ipod"));
        assert!(command
            .output
            .as_path()
            .is_some_and(|path| path.extension().and_then(|ext| ext.to_str()) == Some("m4b")));
    }

    #[test]
    fn ffmpeg_aac_and_alac_commands_pin_mp4_m4a_muxer() {
        let source = SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Flac,
            codec: crate::enums::AudioCodec::Flac,
            sample_rate_hz: Some(44_100),
            bit_depth: Some(PcmBitDepth::Int16),
            true_source_depth: Some(PcmBitDepth::Int16),
            source_representation: Default::default(),
            sample_kind: Some(crate::enums::SampleKind::SignedInteger),
            channels: Some(2),
            duration: None,
            frame_extent: None,
            audio_md5: None,
        };

        let mut aac_settings = PipelineSettings::default();
        aac_settings.target_format = AudioFormat::Aac;
        let aac_request = PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,

            input_path: PathBuf::from("realized.flac"),
            output_path: PathBuf::from("track.m4a"),
            source: source.clone(),
            settings: aac_settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        };
        let aac_step = PlanStep::new(
            0,
            PlanOperation::EncodeLossy {
                target_format: AudioFormat::Aac,
                target_rate_hz: Some(44_100),
                apply_processing: false,
            },
            InputSource::Path(PathBuf::from("realized.flac")),
            OutputSink::Path(PathBuf::from("track.m4a")),
            "Encode AAC output",
        );
        let aac_command = FfmpegPlugin.build_command(&aac_request.context(), &aac_step).unwrap();
        assert!(aac_command.args.windows(2).any(|window| window[0] == "-f" && window[1] == "ipod"));

        let mut alac_settings = PipelineSettings::default();
        alac_settings.target_format = AudioFormat::Alac;
        let alac_request = PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,

            input_path: PathBuf::from("realized.flac"),
            output_path: PathBuf::from("track.m4a"),
            source,
            settings: alac_settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        };
        let alac_step = PlanStep::new(
            0,
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Alac,
                target_rate_hz: None,
                target_bit_depth: PcmBitDepth::Int16,
                apply_processing: false,
            },
            InputSource::Path(PathBuf::from("realized.flac")),
            OutputSink::Path(PathBuf::from("track.m4a")),
            "Encode ALAC output",
        );
        let alac_command = FfmpegPlugin.build_command(&alac_request.context(), &alac_step).unwrap();
        assert!(alac_command.args.windows(2).any(|window| window[0] == "-f" && window[1] == "ipod"));
    }

    #[test]
    fn ffmpeg_metadata_transfer_carries_typed_metadata_effects() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.metadata.transfer_tags = true;
        settings.metadata.preserve_artwork = true;
        let source = SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Dsf,
            codec: crate::enums::AudioCodec::Dsd,
            sample_rate_hz: Some(2_822_400),
            bit_depth: None,
            true_source_depth: None,
            source_representation: Default::default(),
            sample_kind: Some(crate::enums::SampleKind::Dsd),
            channels: Some(2),
            duration: None,
            frame_extent: None,
            audio_md5: None,
        };
        let request = PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,

            input_path: PathBuf::from("source.dsf"),
            output_path: PathBuf::from("output.flac"),
            source,
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        };
        let context = request.context();
        let step = PlanStep::new(
            0,
            PlanOperation::MetadataTransfer {
                target_format: AudioFormat::Flac,
                transfer_tags: true,
                preserve_artwork: true,
            },
            InputSource::Path(PathBuf::from("encoded.flac")),
            OutputSink::Path(PathBuf::from("tagged.flac")),
            "Apply metadata and artwork policy",
        );

        let command = FfmpegPlugin.build_command(&context, &step).unwrap();
        let shared = build_ffmpeg_source_metadata_transfer_command(
            Path::new("encoded.flac"),
            Path::new("source.dsf"),
            Path::new("tagged.flac"),
            &AudioFormat::Flac,
            "flac",
            &[],
            true,
            true,
            None,
            "Apply metadata and artwork policy",
        )
        .unwrap()
        .with_metadata_effect(FfmpegPlugin.metadata_effect(&context, &step));

        assert_eq!(
            command, shared,
            "planner and explicit source-authority metadata rewrites must share one complete planned command shape"
        );

        assert_eq!(
            command.metadata_effect,
            MetadataPlanEffect {
                source_tags_transferred_from_original_source: true,
                artwork_transferred_from_original_source: true,
                ..MetadataPlanEffect::none()
            }
        );
    }

    #[test]
    fn shared_ffmpeg_metadata_rewrite_does_not_open_source_for_strip_only_operation() {
        let command = build_ffmpeg_source_metadata_transfer_command(
            Path::new("encoded.flac"),
            Path::new("source.dsf"),
            Path::new("stripped.flac"),
            &AudioFormat::Flac,
            "flac",
            &[],
            false,
            false,
            None,
            "Strip source metadata",
        )
        .unwrap();

        assert!(command.args.windows(2).any(|window| {
            window[0] == "-i" && window[1] == "encoded.flac"
        }));
        assert!(
            !command.args.iter().any(|arg| arg == "source.dsf"),
            "strip-only metadata rewrites must preserve the prior no-source-read behavior"
        );
        assert!(command.args.windows(2).any(|window| {
            window[0] == "-map_metadata" && window[1] == "-1"
        }));
        assert!(command.args.iter().any(|arg| arg == "-vn"));
    }

    #[test]
    fn ffmpeg_encode_from_original_source_carries_original_source_effects() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.metadata.transfer_tags = true;
        settings.metadata.preserve_artwork = true;
        let source = SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Wav,
            codec: crate::enums::AudioCodec::PcmSigned,
            sample_rate_hz: Some(44_100),
            bit_depth: Some(PcmBitDepth::Int16),
            true_source_depth: Some(PcmBitDepth::Int16),
            source_representation: Default::default(),
            sample_kind: Some(crate::enums::SampleKind::SignedInteger),
            channels: Some(2),
            duration: None,
            frame_extent: None,
            audio_md5: None,
        };
        let request = PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,

            input_path: PathBuf::from("source.wav"),
            output_path: PathBuf::from("output.flac"),
            source,
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        };
        let context = request.context();
        let step = PlanStep::new(
            0,
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Flac,
                target_rate_hz: None,
                target_bit_depth: PcmBitDepth::Int16,
                apply_processing: false,
            },
            InputSource::Path(PathBuf::from("source.wav")),
            OutputSink::Path(PathBuf::from("output.flac")),
            "Encode PCM output",
        );

        let command = FfmpegPlugin.build_command(&context, &step).unwrap();

        assert_eq!(
            command.metadata_effect,
            MetadataPlanEffect {
                source_tags_transferred_from_original_source: true,
                artwork_transferred_from_original_source: true,
                ..MetadataPlanEffect::none()
            }
        );
        assert_eq!(
            FfmpegPlugin.metadata_disposition(&context, &step),
            MetadataDisposition::WritesRequestedPolicy,
            "an FFmpeg encode can make a later MetadataTransfer redundant only when it reads the original request input"
        );
    }

    #[test]
    fn ffmpeg_encode_from_intermediate_preserves_current_input_without_claiming_original_source_transfer() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.metadata.transfer_tags = true;
        settings.metadata.preserve_artwork = true;
        let source = SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Wav,
            codec: crate::enums::AudioCodec::PcmSigned,
            sample_rate_hz: Some(44_100),
            bit_depth: Some(PcmBitDepth::Int16),
            true_source_depth: Some(PcmBitDepth::Int16),
            source_representation: Default::default(),
            sample_kind: Some(crate::enums::SampleKind::SignedInteger),
            channels: Some(2),
            duration: None,
            frame_extent: None,
            audio_md5: None,
        };
        let request = PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,

            input_path: PathBuf::from("source.wav"),
            output_path: PathBuf::from("output.flac"),
            source,
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        };
        let context = request.context();
        let step = PlanStep::new(
            1,
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Flac,
                target_rate_hz: None,
                target_bit_depth: PcmBitDepth::Int16,
                apply_processing: false,
            },
            InputSource::Path(PathBuf::from("intermediate.wav")),
            OutputSink::Path(PathBuf::from("output.flac")),
            "Encode PCM output",
        );

        let command = FfmpegPlugin.build_command(&context, &step).unwrap();

        assert_eq!(
            command.metadata_effect,
            MetadataPlanEffect {
                tags_preserved_from_command_input: true,
                artwork_preserved_from_command_input: true,
                ..MetadataPlanEffect::none()
            }
        );
        assert_eq!(
            FfmpegPlugin.metadata_disposition(&context, &step),
            MetadataDisposition::DoesNotWrite,
            "preserving metadata from an intermediate input must not prune an explicit original-source MetadataTransfer step"
        );
    }

    #[test]
    fn metaflac_source_audio_md5_carries_typed_metadata_effect() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.metadata.store_source_audio_md5 = true;
        let source = SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Flac,
            codec: crate::enums::AudioCodec::PcmSigned,
            sample_rate_hz: Some(44_100),
            bit_depth: Some(PcmBitDepth::Int16),
            true_source_depth: Some(PcmBitDepth::Int16),
            source_representation: Default::default(),
            sample_kind: Some(crate::enums::SampleKind::SignedInteger),
            channels: Some(2),
            duration: None,
            frame_extent: None,
            audio_md5: Some("0123456789abcdef0123456789abcdef".into()),
        };
        let request = PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,

            input_path: PathBuf::from("source.flac"),
            output_path: PathBuf::from("output.flac"),
            source,
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        };
        let context = request.context();
        let step = PlanStep::new(
            0,
            PlanOperation::StoreSourceAudioMd5 {
                target_format: AudioFormat::Flac,
            },
            InputSource::Path(PathBuf::from("output.flac")),
            OutputSink::InPlace(PathBuf::from("output.flac")),
            "Store source audio MD5 metadata",
        );

        let command = MetaflacPlugin.build_command(&context, &step).unwrap();

        assert_eq!(
            command.metadata_effect,
            MetadataPlanEffect {
                source_audio_md5_written: true,
                ..MetadataPlanEffect::none()
            }
        );
    }

    #[test]
    fn dsd_fixed_gain_emits_plain_gain_and_off_emits_nothing() {
        let mut dsd = DsdSettings::default();
        dsd.set_gain_policy(SampleGainPolicy::FixedGain {
            gain_db: "2.250000000".parse().unwrap(),
        });
        let mut args = Vec::new();
        add_sox_dsd_to_pcm_gain(&dsd, &mut args).unwrap();
        assert_eq!(args, vec!["gain", "2.250000000"]);

        dsd.set_gain_policy(SampleGainPolicy::Off);
        args.clear();
        add_sox_dsd_to_pcm_gain(&dsd, &mut args).unwrap();
        assert!(args.is_empty());
    }

    #[test]
    fn dsd_certified_gain_never_falls_back_to_sox_norm() {
        for policy in [
            SampleGainPolicy::TruePeakGuard {
                target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
                scope: crate::enums::TruePeakScope::Track,
                scan: crate::enums::TruePeakScanTier::Fast,
            },
            SampleGainPolicy::TruePeakNormalize {
                target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
                scope: crate::enums::TruePeakScope::Track,
                scan: crate::enums::TruePeakScanTier::Reference,
            },
        ] {
            let mut dsd = DsdSettings::default();
            dsd.set_gain_policy(policy);
            let mut args = Vec::new();
            let error = add_sox_dsd_to_pcm_gain(&dsd, &mut args).expect_err("unresolved certified policy must refuse");
            assert!(matches!(error, PlanningError::CapabilityUnavailable { .. }));
            assert!(!args.iter().any(|arg| arg == "norm"));
        }
    }

    fn dsd_sinc_guard_command(source_hz: u32, target_rate_hz: u32) -> PlannedCommand {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.dither_type = DitherType::None;
        let source = SourceInfo {
            dsd_source_kind: None,

            format: AudioFormat::Dsf,
            codec: crate::enums::AudioCodec::Dsd,
            sample_rate_hz: Some(source_hz),
            bit_depth: None,
            true_source_depth: None,
            source_representation: Default::default(),
            sample_kind: Some(crate::enums::SampleKind::Dsd),
            channels: Some(2),
            duration: None,
            frame_extent: None,
            audio_md5: None,
        };
        let request = PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,

            input_path: PathBuf::from("input.dsf"),
            output_path: PathBuf::from("output.flac"),
            source,
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        };
        let context = request.context();
        let step = PlanStep::new(
            0,
            PlanOperation::DsdToPcm {
                target_format: AudioFormat::Flac,
                target_rate_hz,
                target_bit_depth: PcmBitDepth::Int24,
                lowpass: DsdLowpassMethod::SoxUltra,
            },
            InputSource::Path(PathBuf::from("input.dsf")),
            OutputSink::Path(PathBuf::from("output.flac")),
            "Create PCM output",
        );
        SoxPlugin.build_command(&context, &step).unwrap()
    }

    fn sox_pcm_encode_command_for_depth_case(
        source_depth: PcmBitDepth,
        true_source_depth: PcmBitDepth,
        target_depth: PcmBitDepth,
        dither_type: DitherType,
    ) -> PlannedCommand {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.target_bit_depth = BitDepthTarget::Pcm(target_depth);
        settings.dither_type = dither_type;
        let source = SourceInfo {
            dsd_source_kind: None,
            format: AudioFormat::Wav,
            codec: crate::enums::AudioCodec::PcmSigned,
            sample_rate_hz: Some(44_100),
            bit_depth: Some(source_depth),
            true_source_depth: Some(true_source_depth),
            source_representation: crate::source::SourceRepresentationKind::Pcm,
            sample_kind: Some(crate::enums::SampleKind::SignedInteger),
            channels: Some(2),
            duration: None,
            frame_extent: None,
            audio_md5: None,
        };
        let request = PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,
            input_path: PathBuf::from("carrier.wav"),
            output_path: PathBuf::from("output.flac"),
            source,
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        };
        let context = request.context();
        let step = PlanStep::new(
            0,
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Flac,
                target_rate_hz: None,
                target_bit_depth: target_depth,
                apply_processing: true,
            },
            InputSource::Path(PathBuf::from("carrier.wav")),
            OutputSink::Path(PathBuf::from("output.flac")),
            "SoX carrier-width restoration regression test",
        );
        SoxPlugin.build_command(&context, &step).unwrap()
    }

    #[test]
    fn sox_carrier_width_restoration_disables_only_implicit_dither() {
        for depth in [PcmBitDepth::Int16, PcmBitDepth::Int24] {
            let command = sox_pcm_encode_command_for_depth_case(
                PcmBitDepth::Int32,
                depth,
                depth,
                DitherType::None,
            );
            assert_eq!(command.args[0], "-S");
            assert_eq!(command.args[1], "-D");
            assert_eq!(command.args[2], "carrier.wav");
            assert!(command
                .args
                .windows(2)
                .any(|pair| pair[0] == "-b" && pair[1] == depth.bits().to_string()));
            assert!(!command.args.iter().any(|arg| arg == "dither" || arg == "rate" || arg == "sinc"));
        }

        let real_reduction = sox_pcm_encode_command_for_depth_case(
            PcmBitDepth::Int32,
            PcmBitDepth::Int24,
            PcmBitDepth::Int16,
            DitherType::None,
        );
        assert_eq!(real_reduction.args[0], "-S");
        assert_ne!(real_reduction.args.get(1).map(String::as_str), Some("-D"));

        let configured_dither = sox_pcm_encode_command_for_depth_case(
            PcmBitDepth::Int32,
            PcmBitDepth::Int16,
            PcmBitDepth::Int16,
            DitherType::Tpdf,
        );
        assert_eq!(configured_dither.args[0], "-S");
        assert_ne!(configured_dither.args.get(1).map(String::as_str), Some("-D"));
    }

    #[test]
    fn dsd_noise_strip_sinc_is_skipped_when_cutoff_reaches_target_nyquist() {
        // DSD64 default cutoff is 25 kHz: above the 22.05 kHz Nyquist of a
        // 44.1 kHz target -> sox would reject the filter, so it must be
        // skipped (the rate converter's anti-alias already bandlimits).
        let command = dsd_sinc_guard_command(2_822_400, 44_100);
        assert!(
            !command.args.iter().any(|arg| arg == "sinc"),
            "{:?}",
            command.args
        );

        // DSD256 default cutoff is 96 kHz: above the 44.1 kHz Nyquist of an
        // 88.2 kHz target — the exact shape that failed every real DSD256
        // DSF conversion.
        let command = dsd_sinc_guard_command(11_289_600, 88_200);
        assert!(
            !command.args.iter().any(|arg| arg == "sinc"),
            "{:?}",
            command.args
        );

        // Equality boundary: sox rejects cutoff >= rate/2, so cutoff ==
        // Nyquist must ALSO skip. DSD128 (48 kHz cutoff) -> 96 kHz target
        // and DSD256 (96 kHz cutoff) -> 192 kHz target sit exactly on it;
        // relaxing the guard's `<` to `<=` regresses both to runtime sox
        // failures.
        let command = dsd_sinc_guard_command(5_644_800, 96_000);
        assert!(
            !command.args.iter().any(|arg| arg == "sinc"),
            "{:?}",
            command.args
        );
        let command = dsd_sinc_guard_command(11_289_600, 192_000);
        assert!(
            !command.args.iter().any(|arg| arg == "sinc"),
            "{:?}",
            command.args
        );

        // DSD256 at its default 352.8 kHz target keeps the strip.
        let command = dsd_sinc_guard_command(11_289_600, 352_800);
        assert!(
            command.args.windows(4).any(|w| w == ["sinc", "-a", "180", "-96000"]),
            "{:?}",
            command.args
        );
    }

    #[test]
    fn dsd_to_pcm_directional_sinc_uses_from_dsd_settings() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.dsd.general_from_dsd.lowpass = DsdLowpassMethod::Sinc;
        settings.dsd.general_from_dsd.sinc.taps = 4096;
        settings.dsd.general_from_dsd.sinc.passband_hz = 24_000.0;
        settings.dsd.pcm_to_dsd.sinc.taps = 8192;
        let source = SourceInfo {
            dsd_source_kind: None,
            format: AudioFormat::Dsf,
            codec: crate::enums::AudioCodec::Dsd,
            sample_rate_hz: Some(2_822_400),
            bit_depth: None,
            true_source_depth: None,
            source_representation: crate::source::SourceRepresentationKind::Dsd,
            sample_kind: Some(crate::enums::SampleKind::Dsd),
            channels: Some(2),
            duration: None,
            frame_extent: None,
            audio_md5: None,
        };
        let request = PlanRequest {
            input_path: PathBuf::from("source.dsf"),
            output_path: PathBuf::from("output.flac"),
            source,
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,
        };
        let mut args = Vec::new();
        add_sox_dsd_to_pcm_effects(
            &request.context(),
            &mut args,
            88_200,
            PcmBitDepth::Int24,
            DsdLowpassMethod::Sinc,
        )
        .unwrap();
        assert!(args.iter().any(|arg| arg == "4096"), "{args:?}");
        assert!(!args.iter().any(|arg| arg == "8192"), "{args:?}");
    }

}
