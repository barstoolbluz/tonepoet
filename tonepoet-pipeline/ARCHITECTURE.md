# Architecture

## Boundary

`tonepoet-pipeline` is pure. It takes `PlanRequest` and returns `ConversionPlan`. It does not probe inputs, read config files, allocate temporary files, spawn commands, or mutate the filesystem.

## Flow

1. `PipelineSettings::validate()` checks target combinations and field ranges.
2. `SourceInfo::validate()` checks source facts supplied by the caller.
3. `plan_topology()` creates logical `PlanStep` values.
4. `ToolRegistry` selects a plugin for each step using support scores and `PreferredTool`.
5. The selected plugin turns the step into a deterministic `PlannedCommand`.
6. The caller executes commands and applies `Finalization::AtomicRename`.

## Plugin model

Every tool implements `ToolPlugin`:

- `id()` returns a stable `ToolIdentifier`.
- `supports()` scores a logical `PlanStep`.
- `build_command()` emits argv without side effects.
- `metadata_disposition()` lets a selected plugin declare whether it already wrote the requested tags/artwork policy; the planner uses this after registry selection to skip only proven-redundant metadata rewrites and rewires later in-place post-processing to the selected encoder output.

Built-in plugins:

- `FfmpegPlugin` for decode, FFmpeg encodes, metadata rewrite, and generic verify.
- `SoxPlugin` for PCM processing, SoX-supported encodes, PCM/DSD conversion, and DSD rate changes.
- `SsrcPlugin` for brick-wall or forced SSRC resampling.
- `LoudgainPlugin` for ReplayGain tagging.
- `MetaflacPlugin` for FLAC source-audio MD5 Vorbis comments.
- `FlacPlugin` for native FLAC decode verification.

## Idempotency

All work paths derive from the requested output path, step index, and target extension. No random suffixes, timestamps, or ambient state enter planning. The final plan points callers to atomically rename a completed work file into the requested output path.

## Metadata policy

Passthrough is copy-safe only when the user wants source tags and artwork preserved, no post-processing is requested, source codec facts match the target, and relevant encoder-specific settings are still at copy-safe values. Lossy same-format inputs re-enter the encode path unless future source facts can prove rate-control equality. If the user asks to strip either tags or artwork, or requests ReplayGain, FLAC source-MD5 tagging, or verification, the planner emits a stream-copy rewrite followed by post-processing rather than re-encoding audio.

Metadata/artwork support is target-aware. The topology keeps metadata requests explicit; registry selection then either picks a plugin that can write the requested policy or reports `NoPluginForOperation`. FFmpeg encodes receive credit for metadata handling only when the selected target supports every requested part of the policy.

## Verification policy

Verification stages decode the completed work file. `metaflac --list` is intentionally not used for verification because it inspects metadata rather than decoding audio. FLAC verification uses `flac -t -s` when requested and available via the built-in registry; FFmpeg decode-to-null remains the fallback.

## DSD policy

DSD source detection requires explicit DSD facts: DSF/DFF format, DSD codec, or `SampleKind::Dsd`. DSD-looking PCM rates do not trigger DSD planning. DSD rate changes consume the configured low-pass method before remodulation.

## ReplayGain policy

ReplayGain operations carry the target format. The built-in loudgain plugin supports FLAC, MP3, AAC/M4A, Opus, ALAC, and WavPack. Other formats need ReplayGain disabled or a custom plugin that explicitly supports `PlanOperation::ReplayGain`.

## Custom format policy

Custom target formats are not rejected by topology. The planner emits an `EncodePcm` step with `AudioFormat::Custom`; a caller-registered plugin owns the target command construction and metadata semantics. If the custom encoder writes the requested metadata policy itself, it should return `MetadataDisposition::WritesRequestedPolicy`; otherwise it should also support `PlanOperation::MetadataTransfer` or disable metadata requests before planning. Built-in plugins return unsupported for custom formats.
