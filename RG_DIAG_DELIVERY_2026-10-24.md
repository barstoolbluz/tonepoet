# TonePoet CUE ReplayGain profiling — diagnostic code delivery

**Prepared:** October 24, 2026  
**Purpose:** Establish the source of the ~15-second RG-Off versus ~89-second RG-On CUE album runtime disparity, without assuming the cause and without changing DSP policy or parallelization architecture.  
**Input source:** `TonePoet_SNAPSHOT_da8702a_2026-10-24.tar.gz`, SHA-256 `63e4e4ee4a0ae880ffd0126c6c52350ae374031c9995fb909155f3c4b47a93c5`.

## Deliverable and change scope

This bundle contains the **complete source tree** and a repeatable host-run benchmark harness. The original snapshot has 1,172 regular files. Exactly four *existing* files have changed:

- `src/convert/pipeline/mod.rs` — includes the opt-in measurement module.
- `src/convert/pipeline/tool.rs` — measures the mirrored transport's decoder read / encoder write / FIFO write separately; its **unprofiled transfer loop remains unchanged** apart from a comment.
- `src/convert/pipeline/stages.rs` — correlates transport and observer records to a process-salted run ID and authored track ordinal; records result on both success and failure.
- `src/convert/replaygain.rs` — measures source-pass PCM read/decoding, native loudness and reporting true-peak work with the same algorithms and ordering; adds an instrumented-versus-plain numerical identity test.

All other files present in the source snapshot are byte-identical. `Cargo.lock`, project feature defaults, FFmpeg commands, ReplayGain arithmetic and rounding, the direct stream encoder, output naming, metadata, verification, and safe-publication policies have **not been intentionally modified**.

New files:

- `src/convert/pipeline/rg_profile.rs`: separate, opt-in timing/correlation module; all output goes through the existing optional IP-01 baseline JSONL sink. Disabled unless **both** `TONEPOET_CUE_RG_PROFILE=1` and `TONEPOET_PIPELINE_BASELINE_JSONL` are set.
- `crates/tonepoet-true-peak/examples/rg_native_bench.rs`: independent, bounded-memory PCM microbenchmark using the shipping `IntegratedOnly` native loudness and reporting peak kernels, with per-block and finalization timing and exact result-bit reporting. One to twenty repetitions.
- `tooling/rg_diag/run_host.py`: no-dependency host-run wrapper with optional Linux process-tree CPU lower-bound samples, bounded CUE S1 decoder/encoder role inference without recording command-line arguments, exclusive run labels, and private trace permissions.
- `tooling/rg_diag/analyze.py`: schema-validated JSONL analysis, per-run/track attribution, missing-track-manifest, incomplete-trace and mismatched-byte rejection, observer-recovery fallback differentiation, and non-additivity warnings.
- `tooling/rg_diag/test_analyze.py` and `tooling/rg_diag/test_run_host.py`: adversarial analyzer and command-role unit tests.
- `tooling/rg_diag/source-changes.patch`: normalized four-file baseline patch for audit; verified to re-create the changed files byte-for-byte when applied to the original source.

Start with `RG_DIAG_HOST_RUNBOOK_2026-10-24.md`. This diagnostic package is **not** the conditional S3 per-worker ReplayGain observer and is not an optimization to be merged into the production defaults.

## Verification carried out during preparation

| Gate | Result | Evidence |
|---|---|---|
| Input snapshot SHA-256 | **PASS** | Checksum above; raw archive inspected. |
| Original source scope | **PASS** | 1,172 original regular files compared; exactly four byte-changed, none missing. |
| Source patch reapplication | **PASS** | `patch -p1` on independently extracted original four files, all outputs `cmp` byte-identical. |
| Analyzer negative/positive tests | **PASS** | 25/25 `unittest` tests: mismatch, fallback, missing peer, missing source authority, duplicate, incomplete, malformed schema, overlong JSONL, etc. |
| Host-run wrapper smoke test | **PASS** | Isolated subprocess launch and report on a no-CUE fixture; correctly labels `NOT INSTRUMENTED`. |
| Python syntax | **PASS** | All new Python scripts compile with `py_compile`. |
| Rust compiler / tests / release build | **NOT RUN** | No `rustc` / `cargo` in the preparation container. Host qualification mandatory. |
| Native audio ReplayGain parity | **NOT RUN** | Actual Billie Holiday audio source and audio build host unavailable. |
| Numerical benchmark runtime | **NOT RUN** | Host Rust toolchain and real audio unavailable. |
| Real backpressure / CPU attribution | **NOT INSTRUMENTED HERE** | Only actual host runs can produce evidence; syscall-level blocked time requires additional OS tracing if call-wall attribution is inconclusive. |

This is a **test-ready diagnostic implementation, not a compile-qualified or host-qualified one**. Do not claim measured speedup or an established bottleneck until the host executes the runbook.

## Adjudication criteria

The agent should return a specific evidence-backed answer to *which stage dominates in the exact default conversion*: native loudness arithmetic, reporting true-peak arithmetic, decode/conversion, FIFO write backpressure, scheduling/polling, fallback post-output measurement, or some combination. Use median runtime from repeated, settings-matched uninstrumented trials, and the independent microbenchmark. Native CPU seconds, instrumented wall calls, and end-to-end conversion seconds are different measurements; do not conflate them. An album analyzed continuously by the standalone meter is not identical to independent per-track meters; use separately extracted, frame-exact track PCM files for an exact matched-workload comparison.

Do not alter numerical DSP, remove ReplayGain, enlarge buffers without bounds, or resume S3 implementation as part of this diagnosis. If the measurements are inconclusive, report which single additional OS observation would discriminate between remaining causes.

## Resumption and identity

If the test session is interrupted, continue from the exact source SHA-256 manifest `RG_DIAG_CHANGED_SHA256SUMS.txt`, this delivery note, the host runbook, unmodified raw traces, and `RG_DIAG_REPORT.md` generated by the analyzer. Preserve failing output and all relevant command lines privately; rerun only the failed/incomplete qualification gates. Output directories and trace run labels are intentionally exclusive to prevent accidental reuse or overwriting.
