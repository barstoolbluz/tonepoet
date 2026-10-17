# TonePoet HDCD/CUE R4 — stack-abort corrective handoff

**Date:** 2026-10-17. **Status:** TEST-HARNESS CORRECTED, RUST BUILD-HOST **UNQUALIFIED**.

## Input and evidence boundaries

- Incoming full repository: `TonePoet_LOGGING_HDCD_CUE_R4_2026-10-17.tar.gz`.
- Incoming SHA-256: `d680770b2ec9a1e2f84ef46d125006fa25d0c7be19e637d4a863350172f2c22f`.
- The supplied R4 brief reports two reproducible test-process SIGABRTs for
  `source_target_preserves_float32_cue_sample_class` in `depth_format_matrix`
  and `explicit_single_cue_bypass_keeps_side_album_identity` in
  `unified_synthetic_cue_output_boundary`. Those aborts occurred on the
  requester's Rust build host; this container lacks `rustc`, `cargo`, and Nix.
- The R2 introduction date is an **unbisected inference** from the brief,
  not an independently established causal boundary. Do not silently promote
  it to a proof about the first defective commit.

## Source-level diagnosis

Both failing tests previously used plain `#[tokio::test]` (Tokio's default
**current-thread** runtime, polled on libtest's relatively small test thread).
Both invoke the full CUE `run_pipeline_item` conversion future; a process stack
abort prevents the ordinary test assertion and `test result:` footer entirely.

The repository already distinguishes test stacks from conversion stacks:

- `src/convert/pipeline/scheduler.rs` declares the production worker reservation
  `CONVERSION_RUNTIME_WORKER_STACK_BYTES = 4 * 1024 * 1024`.
- `src/main.rs` configures the application multi-thread Tokio runtime with that
  value. `src/convert/processor.rs` has a focused regression harness that uses
  that same reservation.
- `tests/depth_format_matrix.rs` already runs a large PCM depth/format CUE
  matrix on a production-sized Tokio worker rather than libtest's default stack.

**Most economical working hypothesis:** the two new test aborts are the same
small-test-stack limitation. The relevant paths have large bounded async
continuations; these two fixtures declare no registered effects, so neither
crash by itself demonstrates recursive HDCD decoding or a bad HDCD sample
transformation. This remains a hypothesis pending executable R4 tests. A
4 MiB failure would invalidate it and require investigation of the actual
production conversion continuation, not another blind stack increase.

## Narrow correction

Exactly two **existing integration-test** sources changed, plus one new
fail-closed acceptance script. No production code was modified.

1. `tests/depth_format_matrix.rs`: the named Float32 CUE test is now a normal
   `#[test]` that starts a single 4 MiB thread; that thread constructs a
   Tokio **current-thread** runtime and polls the original async body.
2. `tests/unified_synthetic_cue_output_boundary.rs`: the named explicit
   single-side CUE test uses the identical approach.
3. Both test runtimes also set `thread_stack_size` to the same production
   constant for any runtime-spawned threads. Panic payloads are propagated
   through `join()` and `resume_unwind`, so test assertions still fail normally.
4. `tests/hdcd_cue_stack_gate.sh` executes both exact test names independently,
   requires the real ffmpeg/ffprobe/sox tools instead of silently skipping,
   checks Cargo's exit code, and additionally demands an explicit
   `test result: ok. 1 passed; 0 failed; ...` footer for each target. Aborts,
   no-match filters, compiler errors and missing tools are all FAIL, not zero.

The original async test bodies and all their assertion logic were left
unchanged. The wrappers preserve current-thread Tokio behavior, avoid `Send`
assumptions about a large integration future, and add no overhead to production
conversions. No redesign of CUE, HDCD, metadata, scheduling, or DSP was made.

`SOURCE_DELTA_HDCD_CUE_R4.patch` is against the exact incoming two test files;
it also creates the test gate with executable mode. The patch has been replayed
byte-for-byte and checked for whitespace errors. Older R2/R3 manifests and
reports are historical, not the checksum authority for this new R4 tree.

## Build-host acceptance: required, not optional

From an extracted `tonepoet/` root, with the pinned project dependencies,
FFmpeg/ffprobe/SoX installed and the correct Rust toolchain:

```bash
sha256sum -c SHA256SUMS_HDCD_CUE_R4.txt
cargo fmt --all -- --check
cargo check --workspace
bash tests/hdcd_cue_stack_gate.sh
cargo test --workspace --no-fail-fast --exclude tonepoet-true-peak
```

The gate prints the location of the raw per-target logs and refuses zero-test
passes, skips, process aborts, and failed assertions. Inspect the workspace
raw log and Cargo return code rather than adding only printed `passed`/
`failed` totals. Run the separate `tonepoet-true-peak` crate gate according
to the project's existing slow qualification policy.

Do not rely on a global `RUST_MIN_STACK` setting to mask these test failures:
the targeted tests explicitly reproduce the production worker-stack policy.
Check normal production end-to-end CUE paths on the actual build host as well.
If either R4 exact test **still** aborts at the production-size stack, stop
handoff, inspect the aborting thread and call stack, and correct the production
frame/recursion root cause with the smallest targeted patch. Do **not** simply
raise production stack sizes or exclude the tests.

Only after the tests, full workspace gate, source-lock/Reference requalification,
and applicable physical conversions complete should the code be described as
production-qualified. The exact original Reference qualification process and
hash authority have not been overridden by this patch.

## Verified here, without Rust

- Six FFmpeg/Python/SQLite HDCD regression scripts passed. The continuous-HDCD
  CUE test compared 44,433 frames exactly and showed 390 incorrect-cut sample
  mismatches as a non-vacuity control.
- `bash -n` passes for the targeted gate.
- A mocked Cargo exercise demonstrated accept-one-pass / reject-zero-pass /
  reject-SIGABRT / reject-second-target-SIGABRT behaviors.
- Exact three-file patch replay and executable mode were verified.
- Neither of the two affected Rust binaries nor the full Rust workspace was
  compiled or run **in this environment**. Do not conflate model-based FFmpeg
  witnesses or mock gate outcomes with successful Rust execution.

The five previously requested R5 UI observations, real SACD tag-transfer disc
round trip, and real RF64 album inspection remain USER-SIDE OPEN and are not
changed or claimed by R4.
