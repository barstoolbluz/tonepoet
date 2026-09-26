# Delivery notes — decode-only source passthrough corrective

Date: 2026-09-26
Baseline: bundled head `0e64b20` (`v0.5.3`)

## Result

This delivery fixes the decode-only source passthrough defect and the malformed APEv2 field case described in `BRIEF_decode_only_source_passthrough_2026-09-26.md`.

### Source identity and passthrough

- Direct source carriers preserve decoder-reported codec and demuxer/container identity in `SourceAudioDescriptor`.
- Planner source identity uses those probe facts instead of reconstructing identity from a filename extension.
- APE, Musepack, Shorten, Vorbis/Ogg and TTA remain input-only and map to planner `Custom` source identity, so they cannot satisfy passthrough or stream-copy equality with FLAC or another output.
- Built-in source-format identity also requires a compatible decoder-reported carrier. A codec match alone is insufficient; for example, FLAC audio in a non-FLAC carrier cannot be byte-copied to a native `.flac` target.
- Conversion and editor diagnostics report both the filename extension and decoder-reported codec/container identity when they disagree.

### Malformed APEv2 recovery

- Conversion and the editor share one bounded native APEv2 reader.
- The reader may repair-and-warn on the recorded malformed descriptor flags/reserved bytes and claimed-but-absent header only after strict size/item-count bounds and exact item-table validation succeed.
- Arbitrary invalid UTF-8 still fails closed.
- Recognized front/back cover keys mislabeled as text may recover only when the value contains a recognized image signature, directly or after the conventional filename-NUL prefix.
- Recovered title/album metadata feeds the existing album identity and naming path. Recovered front artwork is staged and then embedded by the existing target-specific post-encode metadata path.
- `MAC ` carrier magic admits the same bounded fallback for legacy APE bytes already mislabeled with a `.flac` filename, so conversion and the editor use one policy.

## Principal implementation surface

Changed files relative to bundled head `0e64b20`:

- `src/convert/pipeline/dvda_realize.rs`
- `src/convert/pipeline/materializer_archive.rs`
- `src/convert/pipeline/materializer_cue.rs`
- `src/convert/pipeline/materializer_dvda.rs`
- `src/convert/pipeline/materializer_single.rs`
- `src/convert/pipeline/plan_bridge.rs`
- `src/convert/pipeline/stages.rs`
- `src/convert/pipeline/types.rs`
- `src/convert/queue_expansion.rs`
- `src/metadata_persistence.rs`
- `src/tui/probe.rs`

No other pre-existing file in the delivered tree differs from the implementation bundle that preceded this packaging-only correction.

## Reference qualification decision

**Reference requalification and reinstall are required on the operator's machine before accepting this closure.**

This delivery changes `src/convert/pipeline/plan_bridge.rs` and `src/convert/pipeline/stages.rs`, both inside the installed Reference source lock named by the brief. `tonepoet-pipeline/src/plan.rs` is unchanged.

The installed Reference report, certification and evidence were **not** edited, regenerated, fabricated or otherwise modified by hand in this delivery. The operator must run the prescribed Reference qualification process and reinstall the resulting execution evidence.

## Validation and operator acceptance

This hosted environment does not provide `cargo`, `rustc`, `rustfmt`, Flox or Nix. The authoritative workspace gate and Reference requalification therefore run on the operator's machine, as prescribed by the brief.

Before release acceptance, the operator should:

1. Run the normal workspace gate.
2. Run the required Reference requalification/reinstallation for the changed source lock.
3. Reproduce the affected decode-only conversion without `--force-encode` and confirm the delivered bytes are the requested target format, the log records the encode path, recovered title/album metadata drives naming/grouping, and the front cover is retained.
4. Verify conversion and editor diagnostics for an intentionally misnamed source report both the filename extension and decoded codec/container facts.

## Packaging-only correction

This replacement archive differs from the previous delivery archive only by the addition of this task-specific delivery note. No source implementation or installed Reference evidence was changed for this correction.
