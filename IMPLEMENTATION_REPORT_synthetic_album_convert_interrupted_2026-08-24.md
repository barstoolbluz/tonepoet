# Implementation report — synthetic merged album `Interrupted — Retry` corrective R2

Date: 2026-08-25
Baseline: `synthetic_album_convert_interrupted_2026-08-24_CORRECTED_bundle.tar.gz`
Original brief: `BRIEF_synthetic_album_convert_interrupted_2026-08-24.md`

## Corrective diagnosis

The first correction correctly kept process-scoped synthetic merged-album CUEs out of durable `QueueExecution` acquisition, but stopped the exemption one layer too early.

`QueueExecutionCoordinator::begin_processing` is both the durable SQLite ownership transition and the publisher of the process-local runtime execution supervisor. A synthetic item intentionally skips that function because it has no v24 row. The first correction nevertheless constructed downstream `RealToolRunner`s with `.with_execution_item(item_id)`. A bound runner snapshots `runtime_item_supervisor(item_id)` when constructed and fails closed at execution time if that capability is absent. Therefore a valid synthetic CUE could pass the missing-row seam and then fail at its first real external-tool invocation.

R2 keeps the original architecture choice — no synthetic v24 row and no weakening of ordinary execution authority — and makes the synthetic execution mode consistent through the real-tool pipeline.

## Production correction

Synthetic classification remains exactly the existing `is_synthetic_cue_album_artifact(...)` predicate.

For a synthetic request, the affected queue/scheduler runner construction sites now use the ordinary unbound `RealToolRunner` returned by `RealToolRunner::new(...)` / `with_version_cache(...)`. For every non-synthetic request, the same runner is still immediately bound with `.with_execution_item(...)`.

The covered sites are:

1. Initial CUE/archive/etc. materialization runner in `src/convert/processor.rs`.
2. Shared scheduler runner helper in `src/convert/pipeline/stages.rs`, covering CUE streaming, fallback realization, track conversion, realized-track encoding, and related scheduler paths.
3. Album post-processing runner in `src/convert/processor.rs`, covering merge and subsequent album stages.

The unrelated ordinary single-file worker in `processor.rs` remains unconditionally execution-bound.

`RealToolRunner` fail-closed semantics are unchanged. R2 does not add a silent fallback when an ordinary bound runner lacks a supervisor.

## Durable authority and lifecycle invariants

Unchanged from the original correction and brief:

- Synthetic temp artifacts are still excluded from v24 persistence.
- Synthetic rows are still discarded on durable load if encountered.
- Dead-scope adoption is unchanged.
- No transient v24 row, journal, lease, or cleanup lifecycle was added.
- Ordinary items still call the installed durable acquisition hook before worker submission.
- The ordinary `begin_processing` CAS remains the execution-authority path for non-synthetic queue work.
- `initial_items.pop_front()` dispatch semantics are unchanged.
- Genuinely unrunnable synthetic work still reaches a real terminal `Failed` instead of structural `Interrupted` retry churn.

## Regression coverage

### Valid synthetic album success path

`runnable_synthetic_queue_item_uses_unbound_tools_and_completes_album` constructs the actual topology at issue:

- two readable physical WAV sides;
- one process-scoped `tonepoet-synthetic-cue-albums/process-*/artifact-*/album.cue`;
- two absolute `FILE` references, one per side;
- forced FFmpeg track conversion;
- album merge enabled.

It uses the repository's existing executable test-tool fixture mechanism and maps fake `ffprobe`/`ffmpeg` executables through the real `RealToolRunner` path. The test asserts:

- durable acquisition hook calls = 0 for the synthetic item;
- a non-version `ffprobe` invocation occurs through the initial unbound materializer runner;
- a non-merge FFmpeg invocation occurs through the shared unbound track runner;
- an FFmpeg concat invocation occurs through the unbound album post-processing runner;
- the queue item finishes successfully rather than merely reaching any terminal state;
- no failed item remains;
- published `merged.wav` exists and is non-empty.

This fixture therefore reaches the supervision boundary that the first correction's missing-member test never exercised.

### Invalid synthetic album guardrail

The existing `synthetic_queue_item_bypasses_missing_durable_row_and_settles_terminally` test is retained unchanged in purpose. Its missing CUE member proves that a genuinely unrunnable synthetic item:

- does not call durable acquisition;
- fails for the real source error;
- reaches terminal `Failed`;
- never returns to `Interrupted` solely because no durable row exists.

### Ordinary durable behavior

`durable_acquisition_failure_preserves_retryable_interrupted_outcome` now also asserts that the ordinary acquisition hook is invoked exactly once.

`queue_request_runner_binding_is_synthetic_only` directly pins runner semantics in the shared stages helper:

- an ordinary request remains execution-bound and fails closed if no item-supervisor capability exists;
- a path recognized by the exact synthetic predicate uses the established unbound process-supervision mode and can execute the same fake tool normally.

## Test fixture reuse

The existing Unix executable-script test helper from `pipeline/tool.rs` was lifted to a `#[cfg(all(test, unix))] pub(crate)` helper so processor/stages integration tests can reuse it. Its implementation and existing tool tests are preserved; production builds do not include the helper.

## Static validation performed here

This execution environment still has no `nix`, `cargo`, `rustc`, `rustfmt`, or `rust-analyzer`, so the brief's Rust gates cannot be run here without violating its explicit Nix-only requirement.

Checks performed locally:

- fake `ffprobe` and `ffmpeg` fixture scripts pass `sh -n`;
- fake ffprobe responses parse as JSON;
- the synthetic streaming producer fixture emits the expected one-second s32le byte count;
- `src/db.rs`, `src/main.rs`, `src/tui/convert_actions.rs`, and `src/convert/queue_expansion.rs` are byte-identical to R1;
- the number of pre-existing `pipeline/tool.rs` tests is unchanged after exposing the shared test helper;
- no trailing whitespace was introduced in touched Rust files;
- production `RealToolRunner` fallback/error behavior was not modified.

## Required downstream gates

Run exactly in the project's Nix development shell:

```sh
nix develop --extra-experimental-features 'nix-command flakes' --command cargo fmt -- --check
nix develop --extra-experimental-features 'nix-command flakes' --command cargo test --workspace
nix develop --extra-experimental-features 'nix-command flakes' --command cargo test --workspace
nix develop --extra-experimental-features 'nix-command flakes' --command cargo check
```

Inspect the complete workspace output and require every `test result:` line to report `0 failed` on both runs. Then perform the operator-owned real LP1/LP2 field conversion and confirm the merged album is produced end-to-end.
