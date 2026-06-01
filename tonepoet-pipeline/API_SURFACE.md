# Public API Surface

This document lists the public API exposed by `tonepoet-pipeline`.

## Modules

`enums`, `error`, `mapping`, `plan`, `plugins`, `settings`, `source`, `tools`.

The crate root re-exports the public items from these modules.

## Enums and methods

- `AudioFormat`
  - Variants: `Flac`, `Wav`, `Aiff`, `WavPack`, `Mp3`, `Aac`, `Opus`, `Alac`, `Dsf`, `Dff`, `Custom { extension, display_name }`
  - Methods: `extension`, `display_name`, `is_dsd`, `is_pcm_lossless`, `is_lossy`, `ffmpeg_encodable`, `sox_encodable`
- `AudioCodec`
  - Variants: `Flac`, `PcmSigned`, `PcmUnsigned`, `PcmFloat`, `WavPack`, `Mp3`, `Aac`, `Opus`, `Alac`, `Dsd`, `Custom(String)`
  - Methods: `is_dsd`
- `SampleKind`: `SignedInteger`, `UnsignedInteger`, `Float`, `Dsd`
- `PcmBitDepth`: `Int8`, `Int16`, `Int24`, `Int32`, `Float32`, `Float64`
  - Methods: `bits`, `is_float`, `sample_kind`
- `BitDepthTarget`: `Source`, `Pcm(PcmBitDepth)`
- `RateTarget`: `Source`, `PcmHz(u32)`, `Dsd(DsdRate)`
- `DsdRate`: `Dsd64`, `Dsd128`, `Dsd256`, `Dsd512`, `Dsd1024`
  - Methods: `hz`, `sox_effect`, `default_pcm_target_hz`, `from_hz`
- `DitherType`: `None`, `Tpdf`, `SlopedTpdf`, `Shibata`, `Lipshitz`, `FWeighted`, `ModifiedEWeighted`, `ImprovedEWeighted`, `Gesemann`, `LowShibata`, `HighShibata`
- `ResampleQuality`: `Low`, `Medium`, `High`, `VeryHigh`, `Ultra`
- `NyquistTransition`: `Gentle`, `Medium`, `Steep`, `Sharp`, `BrickWall`
- `PreferredTool`: `Auto`, `Ffmpeg`, `Sox`, `Ssrc`, `Custom(String)`
- `Mp3Mode`: `Cbr`, `Vbr`, `Abr`
- `AacProfile`: `LcAac`, `HeAac`, `HeAacV2`
- `ReplayGainMode`: `Track`, `Album`, `Both`
- `OpusContentType`: `Auto`, `Music`, `Speech`
- `WavPackMode`: `Normal`, `Fast`, `High`, `VeryHigh`
- `SsrcProfile`: `Insane`, `High`, `Long`, `Standard`, `Short`, `Fast`, `Lightning`
  - Methods: `as_arg`
- `DsdNoiseShaper`: `Clans`, `Sdm`, `Crfb`
- `ModulatorOrder`: `Order4`, `Order5`, `Order6`, `Order7`, `Order8`
  - Methods: `value`
- `DsdFilterPreset`: `Auto`, `Sinc`
- `DsdLowpassMethod`: `Auto`, `SoxUltra`, `Sinc`
- `GainCompensation`: `Auto`, `Linear(f32)`, `Decibels(f32)`, `Disabled`
- `InputSource`: `Path(PathBuf)`, `Stdin`
  - Methods: `as_path`
- `OutputSink`: `Path(PathBuf)`, `Stdout`, `InPlace(PathBuf)`
  - Methods: `as_path`
- `Finalization`: `AtomicRename { from, to }`
- `PlanAction`: `PassthroughCopy { input, output, work_path, cleanup_paths, finalization, reason }`, `Execute { commands, cleanup_paths, finalization }`
- `PlanOperation`: `DecodeToPcm`, `ResamplePcm`, `EncodePcm`, `EncodeLossy`, `PcmToDsd`, `DsdToPcm`, `DsdRateChange`, `MetadataTransfer`, `StoreSourceAudioMd5`, `ReplayGain { target_format, mode }`, `Verify`
  - Methods: `label`
- `TopologyPlan`: `Passthrough`, `Execute`
- `ToolIdentifier`: `Ffmpeg`, `Sox`, `Ssrc`, `Loudgain`, `Metaflac`, `Flac`, `Custom(String)`
  - Methods: `program`, `matches_preference`
- `MetadataDisposition`: `DoesNotWrite`, `WritesRequestedPolicy`
  - Methods: `writes_requested_policy`
- `PlanningError`: `InvalidSettings`, `InvalidSource`, `NoPluginForOperation`, `PluginRejectedOperation`, `UnsupportedFormat`, `RegistryError`
  - Constructors: `invalid_settings`, `invalid_source`, `unsupported_format`, `plugin_rejected`

## Structs and methods

- `PipelineSettings`
  - Fields: `target_format`, `target_sample_rate`, `target_bit_depth`, `resample_quality`, `nyquist_transition`, `dither_type`, `preferred_tool`, `force_encode`, `flac`, `mp3`, `aac`, `opus`, `wavpack`, `ssrc`, `dsd`, `metadata`, `verification`, `replay_gain`
  - Methods: `validate`, `explicit_dsd_rate`
- `FlacSettings`: `compression_level`, `verify`
- `Mp3Settings`: `mode`, `bitrate_kbps`, `vbr_quality`
- `AacSettings`: `profile`, `bitrate_kbps`
- `OpusSettings`: `content_type`, `bitrate_kbps`, `complexity`
- `WavPackSettings`: `mode`, `hybrid`, `hybrid_bitrate_kbps`, `correction_file`
- `SsrcSettings`: `force`, `two_pass`, `insane_mode`, `profile`
- `DsdSettings`: `noise_shaper`, `modulator_order`, `trellis`, `pcm_to_dsd_filter`, `dsd_to_pcm_lowpass`, `dsd_to_pcm_gain_db`, `sinc`, `gain_compensation`
- `TrellisSettings`: `lookahead`, `nodes`, `latency`
- `SincFilterSettings`: `oversample_factor`, `taps`, `passband_hz`, `transition_hz`, `kaiser_beta`, `linear_phase`, `allow_aliasing`
- `MetadataSettings`: `transfer_tags`, `preserve_artwork`, `store_source_audio_md5` (validated against target-aware built-in tag/artwork support)
- `VerificationSettings`: `verify_after_encode`, `prefer_native_flac_verify`
- `ReplayGainSettings`: `mode`, `prevent_clipping` (built-in support is target-aware)
- `SourceInfo`
  - Fields: `format`, `codec`, `sample_rate_hz`, `bit_depth`, `sample_kind`, `channels`, `duration`, `audio_md5`
  - Methods: `is_dsd`, `dsd_rate`, `validate`
- `PlanRequest`
  - Fields: `input_path`, `output_path`, `source`, `settings`, `intermediate_dir`
  - Methods: `context`
- `PlanContext`
  - Field: `request`
  - Methods: `intermediate_path`, `final_work_path`
- `PlannedCommand`
  - Fields: `tool`, `args`, `input`, `output`, `environment`, `expected_duration`, `description`
  - Constructor: `new`
- `ConversionPlan`
  - Field: `action`
  - Constructors/methods: `passthrough`, `execute`, `execute_with_cleanup`, `commands`, `cleanup_paths`
- `PlanStep`
  - Fields: `index`, `operation`, `input`, `output`, `description`
  - Constructor: `new`
- `ToolSupport`
  - Constants: `UNSUPPORTED`, `FALLBACK`, `SUPPORTED`, `PREFERRED`, `CANONICAL`
  - Methods: `new`, `is_supported`, `score`
- `ToolRegistry`
  - Constructors/methods: `empty`, `with_builtin_tools`, `register`, `tool_ids`, `selected_tool_id`, `metadata_disposition_for_step`, `build_command`
- Built-in plugin structs: `FfmpegPlugin`, `SoxPlugin`, `SsrcPlugin`, `LoudgainPlugin`, `MetaflacPlugin`, `FlacPlugin`

## Traits

- `ToolPlugin`
  - Required methods: `id`, `supports`, `build_command`
  - Optional method: `metadata_disposition`

## Free functions

### Planning

- `plan_topology(request: &PlanRequest) -> Result<TopologyPlan>`
- `plan_conversion(request: &PlanRequest) -> Result<ConversionPlan>`
- `plan_conversion_with_registry(request: &PlanRequest, registry: &ToolRegistry) -> Result<ConversionPlan>`

### Settings helpers

- `default_pcm_depth_for_format(format: &AudioFormat) -> PcmBitDepth`

### Mapping helpers

- `soxr_precision`
- `sox_rate_quality_flag`
- `sox_dsd_auto_rate_flag`
- `sox_dsd_lowpass_rate_flag`
- `ffmpeg_cutoff`
- `sox_rolloff`
- `sox_dither_args`
- `soxr_dither_method`
- `ssrc_dither_id`
- `ssrc_profile`
- `ffmpeg_pcm_codec`
- `supports_float`
- `ffmpeg_sample_fmt`
- `ffmpeg_aac_profile`
- `opus_application`
- `sox_mp3_compression`
- `wavpack_compression_level`
- `dsd_shaper_name`
- `requires_sox_dither`

## Type aliases

- `Result<T> = std::result::Result<T, PlanningError>`
