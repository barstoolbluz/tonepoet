# R19 handoff — Issue #67: `tonepoet convert` exit status

Date: 2026-10-09. Based on `origin/main` at `4cde3ee` (the complete uploaded R19 snapshot).

## Scope and behavior

**Changed production file:** `src/main.rs` only. No conversion scheduler, publication code,
queue persistence, Reference source-lock path, tool routing, or album finalization changed.

The processor returns `Ok(())` when the queue scheduler completes, including when individual
queue items fail. Previously the CLI discarded the per-item failure information when deciding
its exit code. The CLI now classifies every submitted queue item **after** the processor runs,
terminal queue synchronization has been attempted, and progress output is drained.

| Queue result | CLI exit | Reason |
| --- | --- | --- |
| Every item `Completed` | 0 | All conversions succeeded |
| Every item `Completed` or `CompletedWithActionErrors` | 0 | Audio published; action errors are explicitly nonfatal warnings |
| One or more items `Failed` | nonzero (1 via `anyhow::Result`) | No false success from a queued failure |
| Any `Partial` item (including `--partial`) | nonzero | Partial output is retained but is not full conversion success |
| Any `Cancelled`, `Interrupted`, or nonterminal item | nonzero | Not all admitted work completed |
| Empty item set | nonzero | Never vacuously succeed (normally refused earlier by CLI planner) |
| Early processor error / terminal queue-sync error | nonzero | Original error propagation and combined-error behavior preserved |

Existing `Conversion complete: <success>/<total> succeeded, <failed> failed` output is retained.
Additional partial/incomplete counts and per-item diagnostics appear when appropriate. The
`run_convert` function returns an error only after attempting the existing durable terminal
queue synchronization. Failed and partial conversions retain their existing publication and
cleanup semantics.

`CliConversionOutcome` uses `ConversionQueue::all_items()` (not only `failed_items()`), so the
exit decision is correct even if a terminal item has not yet been moved into the queue's
completed bucket. Work required for exit classification is linear in the number of queue items.

## Regression tests and release acceptance

Seven tests in `src/main.rs::cli_conversion_exit_tests` cover all-success, nonfatal action
warnings, all-failed, mixed success/failure, partial output, nonterminal/cancellation, queue
settlement independence, and empty input.

On the configured build host from the repository root:

```bash
cargo test --bin tonepoet cli_conversion_exit_tests
cargo test --workspace --no-run
cargo test --workspace
cargo test --test reference_qualification_freshness
cargo build --release
./scripts/smoke_cli_convert_exit_codes.sh "$(pwd)/target/release/tonepoet"
```

The black-box smoke uses a generated real PCM WAV plus a corrupt FLAC. It requires a usable
conversion runtime and checks full success (0), complete failure (nonzero), and mixed success
(nonzero), while verifying actual FLAC output counts. It isolates its HOME/XDG data and
cleans its scratch directory. It is **not** a substitute for the qualified real-SACD tests.

After those gates, repeat the real-SACD Reference runtime-mismatch negative smoke in
`R18_REFERENCE_REQUALIFICATION_HANDOFF_2026-10-08.md` **in an isolated copy**. It should now
return nonzero when Reference promotion is refused; also verify the expected refusal message
and the empty destination. Repeat the qualified positive Reference smoke on the real-SACD build
host per existing release procedure.

## Verification and constraints in the assembly environment

- Archive extraction, Rust source inspection and scoped change review performed.
- All 22 non-Cargo Reference common source-lock files match the installed v18 lock sidecar.
  `src/main.rs` is not one of the locked files. Existing v18 qualification artifacts were
  preserved, not edited or regenerated.
- `bash -n` passed for the supplied regression script.
- **Rust/Cargo/Nix are unavailable in this execution environment; no compilation, Cargo test,
  binary execution, or real-SACD smoke has been completed here.** All build-host gates above
  remain required before release acceptance.

## Interruption/restart

This complete repository archive plus this handoff is the checkpoint. Rehydrate it, verify its
SHA256SUMS and the provided patch against the original snapshot, run the build-host gates, then
update this handoff with actual test output. Do **not** edit signed Reference qualification
sidecars to make a test pass; do not requalify solely because `src/main.rs` changed.
