# Delivery notes: DSD Reference qualification harness and presets

Date: 2026-09-24  
Bundled base head: `018c952`

## Requalification status

**Reference requalification/reinstallation is not required for this delivery.**

This status is derived from a path-level diff of the delivered implementation against the bundled `018c952` tree and comparison with the Reference source-lock inputs declared by `build.rs`.

The implementation changes relative to `018c952` are:

- `crates/tonepoet-wizard/src/lib.rs`
- `crates/tonepoet-wizard/src/presets.rs`
- `crates/tonepoet-wizard/src/types.rs`
- `src/convert/pipeline/materializer_archive.rs`
- `src/convert/pipeline/materializer_cue.rs`
- `src/convert/pipeline/materializer_single.rs`
- `src/main.rs`
- `src/tui/presets.rs`
- `tests/dsd_reference_qualification.rs`

`build.rs` binds Reference common-source qualification to:

- `Cargo.toml`
- `Cargo.lock`
- `build.rs`
- `tonepoet-pipeline/Cargo.toml`
- `tonepoet-pipeline/src/semantic_plan.rs`
- `tonepoet-pipeline/src/dsd_reference.rs`
- `tonepoet-pipeline/src/dsd_album_gain.rs`
- `tonepoet-pipeline/src/plan.rs`
- `tonepoet-pipeline/src/settings.rs`
- `tonepoet-pipeline/src/enums.rs`
- `tonepoet-pipeline/src/source.rs`
- `tonepoet-pipeline/src/tools.rs`
- `tonepoet-pipeline/src/fingerprint.rs`
- `tonepoet-pipeline/src/qualification_schema.rs`
- `tonepoet-pipeline/src/w64.rs`
- `src/convert/pipeline/plan_bridge.rs`
- `src/convert/pipeline/track_executor.rs`
- `src/convert/pipeline/stages.rs`
- `src/convert/pipeline/manifest_builder.rs`
- `src/convert/replaygain.rs`

It separately binds the complete `crates/tonepoet-true-peak/src` tree plus `crates/tonepoet-true-peak/Cargo.toml`.

None of the nine implementation changes intersects either locked source set. Therefore the installed v18 Reference qualification report, certification, and evidence remain valid with respect to this delivery's source-lock policy and do not need to be regenerated or reinstalled solely because of these changes.

## Evidence integrity

The installed v18 Reference qualification report, certification, and evidence were **not edited by hand** in this delivery. The complete diff against the bundled `018c952` tree contains only the nine implementation paths listed above, plus this packaging-only delivery note in the corrected archive.

## Validation status

The normal operator-side gate was **not run in the delivery environment**. This environment does not provide the project's Rust/Nix operator toolchain, so no claim is made that the workspace gate or the full 3,540-cell qualification completed here.

The operator should run the normal gate on the intended qualification machine as required by the brief. Because no Reference source-lock input changed, a successful normal gate does not by itself require regeneration or reinstallation of the existing v18 qualification report/certification/evidence.

## Packaging correction

This file is the task-specific delivery note required by `BRIEF_dsd_reference_harness_presets_2026-09-24.md`. No implementation file was changed as part of this packaging correction.
