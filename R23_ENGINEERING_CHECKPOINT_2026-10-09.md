# TonePoet R23 engineering handoff — 2026-10-09

**Release status: IMPLEMENTED, NOT BUILD-QUALIFIED. NOT FOR QUALIFIED REFERENCE RELEASE YET.**

This repository is a full-source corrective bundle for the attached `BRIEF_LOGGING_R23_single_file_log_label_43_49_71_2026-10-09.md` against the attached R23 source baseline. The changes were scoped to five existing files (listed below) plus this handoff, a source diff, the brief, and a fresh R23 source-tree checksum manifest. Reference qualification sidecars have NOT been modified or reissued.

## Corrections implemented

1. **N=1 conversion log renewal (#70 residual).** `src/convert/pipeline/stages.rs` distinguishes replacement of a previously published audio member from addition of a distinct sibling. A redo installs the new standalone `conversion.log` instead of appending it to the old report, with a history snapshot of exactly the previous report. The snapshot and log replacement are registered with the existing durable incremental rollback journal; crashes and failed publications can remove new history and restore the prior visible log. Distinct sibling appends retain their existing behavior; fragment-based N=2/N=3 album assembly is unchanged. Regression: `r23_one_file_redo_refreshes_log_and_archives_only_the_displaced_report` asserts three full conversions, exact bytes, dated history (0/1/2), strict inventory, and current audio content. `r23_incremental_history_is_idempotent_and_removed_on_rollback` tests duplicate suppression and rollback.

2. **Output Options source failure pill.** `src/tui/app.rs` and `src/tui/draw_output_options.rs`: `source track fails` with `fail source` / `keep successes`, distinguishing tracks within one source from independent files. The persisted `partial` boolean and `--partial` CLI flag are unchanged. The rendered-options regression now checks the new label and both outcomes.

3. **#43 recurrence recorded, not prematurely closed.** `docs/OUTSTANDING_ISSUES.md` marks the previously claimed v0.5.3 fix as superseded by the observed SACD ISO Browse context-menu recurrence. R22C's existing code and automated regression remain untouched. A live SACD ISO *Browse -> Convert -> preset* gesture is STILL required before declaring #43 resolved; the six fields must apply without refusal and the actual conversion must use the qualified Reference pathway.

4. **#49 human gain precision.** `stages.rs` formats DSD album gain, loudest peak, and target using integer rounding to 0.001 dB/dBTP in human-readable log strings ONLY. Original `DbNano` values and structured execution evidence are preserved exactly. Regression: `r23_human_dsd_gain_values_are_rounded_only_at_the_display_boundary`, including overflow boundary behavior.

5. **#71 strict publication inventories.** `stages.rs` includes whole-album overwrite and keep-both tests (3 tracks, with logging on/off), plus independently dispatched 2-track folder overwrite, asserting *all* filesystem entries including the numbered keep-both sibling and precisely one previous-report archive when logging applies. `scripts/smoke_r20_album_publication.sh` now recursively asserts an exact closed destination inventory on greenfield, N=1/2/3 overwrite, unlogged single-file redo, and keep-both, rather than only file counts / backup name checks.

## Changed existing files

- `src/convert/pipeline/stages.rs`
- `src/tui/app.rs`
- `src/tui/draw_output_options.rs`
- `scripts/smoke_r20_album_publication.sh`
- `docs/OUTSTANDING_ISSUES.md`

## Validation performed here

- `bash -n scripts/smoke_r20_album_publication.sh`: **PASS**.
- Strict smoke inventory function isolated with a synthetic clean album: **PASS**; injected hidden artifact **REJECTED**.
- Source-level comparison against the original archived repository: exactly the five existing files above were deliberately changed (no manifest, certificate, Reference gate, or other source path changed).
- All deliverable paths and payload checksums are enumerated by `R23_SOURCE_TREE_SHA256SUMS.txt` and checked by the standalone archive-integrity QA.
- **NOT RUN**: Rust compilation, Cargo workspace tests, 3-hour Reference qualification, black-box TonePoet CLI conversions, and live TUI ISO preset gesture. `cargo`, `rustc`, `rustfmt`, and `nix` are unavailable on this execution host. Network installation was attempted and was unavailable. None of these missing checks should be reported as passing.

## Required build-host acceptance, in order

Use the configured real-tool build host and a *real* SACD ISO (not a synthetic fixture). After applying any compiler/test-driven correction, **requalify the final bytes**, not an earlier intermediate state.

```bash
unset TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE
cargo test --lib r23_one_file_redo_refreshes_log_and_archives_only_the_displaced_report
cargo test --lib r23_incremental_history_is_idempotent_and_removed_on_rollback
cargo test --lib r23_human_dsd_gain_values_are_rounded_only_at_the_display_boundary
cargo test --lib issue_71_strict_inventory_covers_album_overwrite_and_keep_both
cargo test --lib issue_71_independent_folder_overwrite_has_an_exact_complete_inventory
cargo test --lib r22_folder_iso_reference_preset_applies_only_after_dsd64_probe_facts
# Existing script also runs complete release/reference, workspace, and N=1/2/3 shell smoke gates.
./scripts/requalify_reference_r20.sh /absolute/path/to/real-SACD.iso
```

Then perform the *actual TUI* Browse-context-menu conversion against the SACD ISO using the stored SACD-to-PCM Reference preset. Confirm every requested applicable field survives source probing; there are no six-field refusals and the actual Reference terminal executes. Record the result in issue #43 before marking it fixed. Complete the **negative runtime mismatch smoke in an isolated scratch checkout** described in `R20_REFERENCE_REQUALIFICATION_HANDOFF_2026-10-09.md`. Do not use an unqualified override.

## Qualification-source lock

The installed source-lock expectation for `src/convert/pipeline/stages.rs` is `ef02d289fb80a2dc7ded4c0aa599b709ba485277726fa20d9e4e90f5665543ee`. The R23 source is `46543dc46d068f3de4220b54ac7e5b88e06d08349f62ccfafac7ffbb072843e2`. This mismatch is **expected before requalification and must remain a release blocker**. The installed certification, report, evidence, and source-lock JSONs are intentionally unchanged pending the real build-host gate.

## Resume / provenance

- `R23_SOURCE_DIFF_2026-10-09.patch`: unified minimal patch against exactly the uploaded R23 source tree.
- `R23_SOURCE_TREE_SHA256SUMS.txt`: SHA256 of every bundled file (excluding itself) for reproducible integrity checks.
- `BRIEF_LOGGING_R23_single_file_log_label_43_49_71_2026-10-09.md`: authoritative task brief copied without edits.
- This checkpoint: exact implementation/test intent and what remains unverified. If a session is interrupted, compare the new workspace files to this manifest and resume with the six focused tests above; **do not assume Reference sidecars are fresh**.
