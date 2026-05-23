# tonepoet-pipeline

`tonepoet-pipeline` is a pure Rust planning crate for tonepoet audio conversions.

It owns the single conversion settings type and unified enum domain that replace the prior `ConversionOptions`, `EncodeOptions`, backend `ConversionSettings`, and `Main*` bridge types. It accepts already-probed source facts and target settings, then returns deterministic command descriptions. It does not spawn processes, probe files, read configuration, or write the filesystem.

## Core guarantees

- Same `PlanRequest` plus same `ToolRegistry` produces the same `ConversionPlan`.
- Every settings field feeds validation, topology, plugin selection, or command construction.
- Passthrough is explicit and selected only when source format, codec class, rate/depth targets, metadata policy, and encoder-specific settings prove a copy is safe.
- Output writes target deterministic work paths; callers atomically rename the completed work file and can use `ConversionPlan::cleanup_paths()` to delete known work files after success, failure, or interruption.
- SSRC, FFmpeg, SoX, loudgain, metaflac, and flac verification are plugins behind one trait.
- FLAC source-MD5 storage uses a Vorbis-comment tag via `metaflac --set-tag=SOURCE_AUDIO_MD5=...`; it never writes ID3v2 tags to FLAC.

## Main entry points

```rust
use tonepoet_pipeline::{plan_conversion, PlanRequest, PipelineSettings};

let plan = plan_conversion(&PlanRequest {
    input_path: "in.flac".into(),
    output_path: "out.flac".into(),
    source,
    settings: PipelineSettings::default(),
    intermediate_dir: Some("work".into()),
})?;
```

The caller owns execution. A `PlannedCommand` contains the selected tool, argv vector, logical input/output, optional environment, progress estimate, and description.

## Metadata and verification behavior

The planner treats metadata policy as part of correctness. A same-format copy is allowed only when `metadata.transfer_tags` and `metadata.preserve_artwork` are both true, no post-processing is requested, and source codec facts prove the requested target already matches. Lossy same-format inputs do not passthrough because the source facts do not prove bitrate, profile, or quality equality. If the user asks to strip tags or artwork, the planner emits a deterministic FFmpeg stream-copy metadata rewrite rather than silently preserving unwanted metadata. ReplayGain-only, FLAC-MD5-only, and verify-only requests use stream-copy planning before post-processing rather than re-encoding audio.

Metadata support is target-aware. Built-in planning reports `NoPluginForOperation` for artwork preservation on formats where the bundled command builders cannot apply it safely. FFmpeg encoders receive credit for metadata handling only when the selected target supports every requested piece of the policy; otherwise an explicit metadata step remains or planning reports that no plugin can satisfy the request.

Verification means decoding the encoded file. FLAC can use the `flac -t -s` plugin when `verification.prefer_native_flac_verify` is true; other targets use FFmpeg decode-to-null validation.



For encode and post-processing paths, metadata-transfer pruning happens only after `ToolRegistry` selects the actual encoder plugin. A SoX-selected encode therefore keeps an FFmpeg metadata transfer step unless the selected plugin explicitly reports `MetadataDisposition::WritesRequestedPolicy`. When the planner prunes a redundant metadata step, it rewires later in-place MD5, ReplayGain, and verification steps to the selected encoder output.

## DSD behavior

DSD target rate lives only in `PipelineSettings::target_sample_rate` via `RateTarget::Dsd`, so there is no duplicate DSD rate field. DSD source classification uses explicit container/codec/sample-kind facts; a PCM stream at a DSD-like sample rate is not treated as DSD by coincidence. DSD low-pass mode and sinc transition width shape the SoX command line, so those settings are not decorative.

## Rust checks

Run before merging:

```text
cargo fmt --check
cargo check --all-targets --all-features
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

This sandbox did not include `cargo` or `rustc`, so the bundle includes static checks only.

## Custom targets

`AudioFormat::Custom` is routed through `ToolRegistry` as an `EncodePcm` operation. Built-in tools intentionally do not claim custom targets; caller-registered plugins can build them without changing planner topology. Custom plugins can also declare metadata support through `metadata_disposition()` or implement explicit metadata/ReplayGain steps.
