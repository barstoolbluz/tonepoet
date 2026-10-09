# TonePoet R22 — engineering handoff (2026-10-09)

## Authority, scope, and status

Authority: `BRIEF_LOGGING_R22_duplicate_log_debug_leakage_iso_preset_2026-10-09.md`.
Input is the complete R21-qualified repository snapshot supplied as
`TonePoet_LOGGING_R22_duplicate_log_debug_leakage_iso_preset_2026-10-09.tar.gz`.
R21's reported `origin/main` was `c08886d`; the supplied snapshot did not include
`.git`, so do not treat that revision as independently verified here.

**Status: source-level R22C test correction prepared, not build-host qualified.**
The execution environment has Python, Bash and Git but lacks `rustc`, `cargo`,
`rustfmt` and Nix. No Rust compilation, unit tests, actual ISO conversion,
full-workspace performance measurement, or certified Reference requalification
was run. The shipped four installed Reference sidecars intentionally remain
untouched and **do not authorize R22 as a qualified Reference build**.

## Code delta — four existing files

1. `src/convert/pipeline/stages.rs` (#70): removed unconditional snapshots after
   publication/final human-log update. Preserve the previous visible
   `conversion.log` **before** displacement, including transactional backup
   cleanup and whole-album replace. History filenames derive their UTC time
   from the displaced report's `Generated (UTC)` header; for legacy reports,
   use the displaced file's modification time. A content-hash suffix safely
   distinguishes different reports generated within the same second. Exclusive
   create, syncing, and existing-content detection avoid replacing archive
   files on retries. Existing archive detection first checks file sizes, reducing
   avoidable reads as history grows. The first greenfield logged run leaves
   only the current `conversion.log`.

2. `src/convert/pipeline/execution_evidence.rs` (human log): represent typed
   planner values using stable, listener-readable terms. The PCM encode line
   becomes `Encoding`; it identifies physical input storage precision, the
   sample-value domain, destination codec/precision, sample/quantization owner,
   processing, the dither algorithm, and dither owner. Composite routes such
   as `SoX → FFmpeg` preserve the distinction between sample work and final
   packaging. Each fresh operation is projected once; generic planner fields
   consumed during projection are not reconstructed on a second application.
   No user-facing parameter is generated from Rust `Debug` enum spelling.
   Diagnostic text for unmet source facts now uses their typed key and reason.
   Raw Rust enum `Debug` remains solely in the separate portable structured
   execution record for action-phase/status machine data, not the human log.

3. `src/tui/command.rs` (Browse SACD ISO): a generic `.iso` with no completed
   probe facts defers the queued preset even if its placeholder source mode
   does not claim a probe is currently in progress. The matching-generation
   completion refreshes actual source identity before applying a preset.
   Missing ISO source facts fail with a probe-identity message rather than
   falsely rejecting all DSD fields. A proven non-DSD ISO still invokes strict
   application and rolls it back atomically on a DSD-preset refusal. Stale
   generations cannot consume the continuation. **No extension-based `.iso`
   implies DSD shortcut was added.**

4. `scripts/smoke_r20_album_publication.sh`: retained broad R20 publication
   coverage while changing first-run expectations to zero dated logs, and
   requiring exactly one second-run archive with byte identity equal to the
   original displaced report for N=1, 2 and 3 album variants.

There are no dependency changes, no new persistent services, no changes to
Audio/Reference math, and no modifications to the qualification sidecars.

## New/updated tests

- `r22_history_archives_only_displaced_reports_with_original_run_time`:
  first-run absence, second-run displacement, original timestamp, same-second
  collision protection, idempotent retry and whole-album history carry.
- `r22_direct_encode_preserves_sample_facts_without_debug_or_duplicate_fields`:
  24-bit/96-kHz direct FLAC, unchanged samples, explicit dither none,
  sample/quantization and dither ownership, and no duplicated display fields.
  A former second-projection equality assertion was removed in R22C: it
  exercised an unsupported internal reuse rather than production behavior.
- `r22_folder_iso_reference_preset_applies_only_after_dsd64_probe_facts`:
  Browse folder-expanded ISO, no early DSD source hint or premature preset,
  ignores stale generation, then applies all six DSD fields to probed DSD64.
- `r22_proven_pcm_iso_keeps_strict_preset_refusal_atomic`:
  known PCM ISO cannot take a Reference DSD preset, and source format policy
  is preserved when the strict application is refused.
- `r22_iso_probe_failure_does_not_interpret_reference_preset_against_hint`:
  failed probe gives a clear identification error, no partial preset.
- Existing `#65` recursive output inventory tests and R20/21 log consent
  tests invert the obsolete first-run archived-log expectation.

The ISO unit tests simulate the reducer's probe-completion boundary; they are
**not** substitutes for a real ISO/DSD decoder or interactive Browse smoke.

## Local static QA

`R22_STATIC_QA_2026-10-09.txt` records **20/20 passing offline checks**:
three Rust-file lexical delimiter checks (not parsing or compiling), presence
of all R22 regression cases, absence of Debug-format strings in human evidence,
expected file-change scope, unchanged Reference sidecars and deliberately
unmatched locked stages.rs digest, and shell-script parsing. These checks
are not a substitute for `cargo test` or the real Reference gate.

## Build-host acceptance (mandatory before release)

At the **repository root** in a configured tool environment:

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo test --lib r22_
cargo test --lib terminal_realization_evidence_preserves_physical_quantization_truth
cargo test --lib issue_65_postconversion_destination_inventory_covers_source_routes_and_log_consent
cargo test --lib requested_human_log_does_not_publish_implicit_machine_evidence
cargo test --lib explicitly_requested_machine_evidence_stays_outside_destination_tree
cargo test --workspace
cargo build --release
./scripts/smoke_r20_album_publication.sh ./target/release/tonepoet
```

`cargo fmt --all` is intentionally before qualification because the local
container had no rustfmt. Review the formatter diff and ensure it is scoped to
the R22/R22C modifications (avoid unrelated formatting churn). Then run:

```bash
./scripts/requalify_reference_r20.sh /absolute/path/to/REAL_SACD.iso
```

This existing gated script executes real-tool Reference certification and
validates the four freshly generated coherent sidecars and source hashes,
then runs comprehensive workspace and compiled-CLI gates plus a real SACD
positive conversion. Its own handoff also requires the isolated negative
runtime-mismatch smoke; **perform and record that** before release.

Also reproduce the reported Browse UI scenario using an actual SACD ISO:
Browse → select its containing folder → Convert → select user Reference
SACD/DSD64-to-PCM-FLAC preset. Confirm no `refused fields: dsd_*`, that the
Reference route is selected, and that audio output passes the qualification
and expected quality verification. A nonspecific/non-SACD ISO must not
acquire DSD privileges.

## Requalification authority

`src/convert/pipeline/stages.rs` participates in
`REFERENCE_COMMON_SOURCE_PATHS` and has changed. The source-lock digest for
that file in the installed `dsd_reference_common_v18_source_lock.json` is
stale after R22. Do not manually rewrite JSON hashes or use the
`TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE` escape hatch. The R20
requalification script in `scripts/` is unchanged and should be run after
final source formatting, on the configured real-tool build host with a real
ISO. The `execution_evidence.rs` and TUI changes do not themselves belong
in `REFERENCE_COMMON_SOURCE_PATHS`, per the R22 brief.

## Portability, integrity, and resumption

- This is a complete, independent source tree rather than an isolated patch.
- `R22_SOURCE_TREE_SHA256SUMS.txt` lists every other file in the tree; verify
  from the repository root with `sha256sum -c R22_SOURCE_TREE_SHA256SUMS.txt`.
- The R22C companion test-only Git patch applies to the original R22 source bundle,
  not an arbitrary repository revision. The separately supplied R22 patch remains
  the original R21-to-R22 change and does not contain the R22C test correction.
- `R22_RESUME_CHECKPOINT_2026-10-09.md` and
  `R22C_TEST_ONLY_HANDOFF_2026-10-09.md` record state and unfinished gates.
- No test outputs have been fabricated. Rust and real-media checks remain
  release blockers until the qualified host produces actual passing evidence.
