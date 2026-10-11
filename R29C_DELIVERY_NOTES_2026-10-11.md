# TonePoet R29C — focused corrective delivery (2026-10-11)

**Status: corrected complete source and static verification only; NOT build-accepted or qualified.**

This corrective applies to the original delivered R29 source (commit `fe3bc13`) and does not redesign R29. Corrected commit: `2d53a58`.

## Exactly two source files changed

1. `tests/settings_sentinel.rs`: the intentionally exhaustive `rich_common_settings()` initializer now explicitly specifies `pcm_true_peak_target_origin` and `dsd_true_peak_target_origin` as `TruePeakTargetOrigin::Unknown`. It does **not** use struct-update defaults and does not change the schema, fingerprint, or fixture's active processing settings. The other two exhaustive constructors (TUI and `Default for PipelineSettings`) already had both fields.
2. `src/convert/pipeline/stages.rs`: the conversion-log fragment now captures a **presentation-only**, optional `reference_delivery_description` from completed, successful `CommandRecord` entries indexed by a delivered `reference-package-output` operation. The source-path identity must match the staged file; a standalone FFmpeg package attributes FFmpeg, and the established SoX-to-FFmpeg producer/consumer package attributes the FFmpeg consumer. Direct Wave64 delivery is attributed only when the completed `reference-terminal-realization` output path is the staged final file, with no separate package operation. Missing, failed, ambiguous or unmatched physical receipts yield no invented attribution. Regular PCM projections and forensic `ExecutionBackend`, `OperationRecord`, `CommandRecord`, and Reference DSP remain unchanged. The human log still prints exactly one delivered path, including its format when recorded, and cannot call a failed track `Written to`.

## Targeted tests added

`r29c_reference_flac_writer_uses_completed_package_command_not_terminal` verifies fragment construction from completed records, a single FFmpeg package, a SoX-to-FFmpeg package, unrelated prefix command indices, nonattribution on a failed consumer, one printed destination, preservation of source execution evidence, and nondelivery wording on failure.

`r29c_direct_reference_wave64_writer_is_terminal_only_when_it_delivers_file` verifies single SoX Wave64 delivery, two-step SoX-to-FFmpeg direct delivery, matching staged-file identity, and suppression on stale/out-of-range invocation indices.

Existing R29 tests covering ordinary PCM typed encoder attribution and the exclusion of the Reference PCM terminal from the FLAC encoder label are retained.

## Static checks performed

- `git diff --check`: PASS.
- Pygments Rust lexer delimiter-balance scan across the changed source and R29 files: PASS. This is a **static delimiter check, not a Rust parse or compilation**.
- Exhaustive `PipelineSettings {` reference inventory across `src/`, `tests/`, `tonepoet-pipeline/src`, and `tonepoet-pipeline/tests`: other constructors already supply the two fields; no additional missing initializer identified.
- No changes to certified sidecars, audio DSP, qualification code, model fingerprints, or settings schema.
- Reproducibility: standalone `R29C_FROM_R29.patch` reproduces the two exact corrected files on the original R29 delivered source (see checkpoint).

**Not run:** Rust compilation, Cargo tests, `cargo fmt`, release build, Reference requalification, source-lock freshness gate, real SACD positive smoke, runtime-mismatch negative smoke, performance benchmarking. This execution environment has no Rust/Cargo/Nix toolchain. None are represented as passed.

## Build-host acceptance instructions

In extracted archive root `TonePoet_R29C/`:

```bash
# Recommended: verify and perform necessary formatting before source locks.
# Do not mass-format unrelated code; any necessary source formatting must
# precede the transactional qualification.
bash R29C_ACCEPTANCE.sh /path/to/real-reference-fixture.iso
```

`R29C_ACCEPTANCE.sh` first runs `cargo test --locked --test settings_sentinel` and `cargo test --locked --lib r29c_`, then delegates to the existing `R29_ACCEPTANCE.sh`, which executes R29 tests and `scripts/requalify_reference_r20.sh`. That repository runner performs coherent sidecar installation and workspace/release/freshness checks. Supply and verify a real SACD fixture and run the existing negative runtime-mismatch smoke. **Do not** disable freshness checks, hand-edit sidecars, or bypass certification. If the documented staging lease-ownership flake recurs, isolate it before determining whether R29C introduced a regression.

Existing four v18 Reference qualification sidecars are retained unchanged in this archive and are **stale** relative to the modified locked sources until requalification completes. This source must not be represented as a qualified release yet.
