# Persistent continuation checkpoint — HDCD/CUE R4 stack corrective

**STATE:** Minimal correction applied and full-source artifact assembled.
Two aborting CUE integration tests now have a production-sized test thread
and Tokio runtime; a separate two-target gate fails closed on aborts and
empty test runs. Rust qualification is NOT DONE. Do not call R4 shipping-ready
until the build-host gates in the R4 handoff complete.

## Identity and authority

- Input: `TonePoet_LOGGING_HDCD_CUE_R4_2026-10-17.tar.gz`.
- Input SHA-256: `d680770b2ec9a1e2f84ef46d125006fa25d0c7be19e637d4a863350172f2c22f`.
- Current work: `TonePoet_HDCD_CUE_R4_STACK_CORRECTIVE_UNQUALIFIED_FULL_REPO_2026-10-17.tar.gz`.
- Governing R4 inventory: `SHA256SUMS_HDCD_CUE_R4.txt` in the `tonepoet/`
  root. Older R2/R3 inventories are historical and **not** valid over these
  altered tests.
- Exact patch: `SOURCE_DELTA_HDCD_CUE_R4.patch` (two integration tests,
  plus an executable `tests/hdcd_cue_stack_gate.sh`).

## What changed — and what did not

Existing files changed: only
`tests/depth_format_matrix.rs` and
`tests/unified_synthetic_cue_output_boundary.rs`.
New executable test gate: `tests/hdcd_cue_stack_gate.sh`.
Other additions: these R4 documentation/verification/checksum artifacts.

Original test assertions are retained, and the async test-body code is
unchanged. No production source, planner, scheduler, HDCD effect,
Reference certificate, or DSP files have been edited.

## Completed checks

- Six original FFmpeg/Python/SQLite HDCD validation scripts: 6/6 passed;
  details `R4_HDCD_CUE_SMOKE_LOG.txt`.
- CUE: 44,433 sample-exact reconstructed frames; 390 mismatched samples for
  the purposely incorrect cut-before-HDCD route.
- Shell syntax and mock Cargo fail-closed scenarios: PASS. See
  `R4_STACK_GATE_MOCK_VERIFICATION_LOG.txt`.
- Source patch: Git-style patch replay byte-exact for each source and the new
  executable test script; `git diff --check` PASS.
- New input-to-output member audit and checksum inventory: see the R4
  verification ledger; preserve the one original relative fixture symlink.

## Not done, do not infer

- Rust compilation, formatting, the two target Rust test executions,
  full workspace tests, remaining known contention flake adjudication.
- Determining by stack sampling/backtrace exactly which continuation/frame
  overflowed. The 2 MiB vs 4 MiB explanation is strong but unproven.
- Build-host physical CUE/HDCD conversions and measured stack/performance
  regression proof.
- Pinned Nix/Reference requalification. No certificate/source-lock changes.
- Five R5 UI checks; physical SACD round trip; large RF64 real-media opening.

## Required next actions

1. Verify the outer SHA256 sidecar and R4 in-tree SHA256 manifest.
2. On a Rust host run `cargo fmt --all -- --check`, `cargo check --workspace`,
   and `bash tests/hdcd_cue_stack_gate.sh`. The script requires actual audio
   tool availability, real one-test result footers, and non-aborted exits.
3. If either still SIGABRTs, analyze *which thread* overflows and the actual
   production stack use before editing production code. Do not increase stack
   size above the existing production bound solely to make the tests pass.
4. Run workspace `--no-fail-fast` gate, preserve full logs and exit status,
   classify any unrelated contention flake only on reproduction evidence.
5. Perform the application's physical CUE conversion checks and unchanged
   Reference-source lock requalification, then update the verification ledger
   and archive if a further source correction proves necessary.

**Scope discipline:** Avoid speculative source refactors; do not import
unrelated R5 UI, SACD, RF64, FFmpeg effect, or planner work into this stack
fix. Repeatability, truthful gate status, and retained conversion behavior
matter more than architectural novelty.
