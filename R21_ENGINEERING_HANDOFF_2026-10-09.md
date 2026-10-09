# R21 corrective handoff — 2026-10-09

## Scope and disposition

**Five existing files modified. R20 conversion and publication semantics are unchanged.** The superseding brief describes eight failures (seven Rust tests and one black-box smoke assertion), plus two non-failing compiler warnings. All six items are addressed locally:

| Brief item | File | Correction |
| --- | --- | --- |
| #65 strict Reference inventory (1 failing Rust test) | `src/convert/pipeline/stages.rs` | Invoke the existing `expected_r20_history_entry` helper in the Reference case, as already done by the other strict inventory tests. It proves exactly one human-log snapshot, identical to `conversion.log`, while rejecting all other unexpected files. |
| ReplayGain smoke (1 failing script assertion) | `scripts/smoke_r20_album_publication.sh` | Generate 44,100 frames at 44.1 kHz (1.0 s) instead of 11,025 (0.25 s). This exceeds the 400 ms gating block. No ReplayGain product behavior changed. |
| Output Options field cycling (2 failing Rust tests) | `src/tui/app.rs` | Reflect the two new `Partial` and `IfExists` fields; expose `Actions` from pane height 20 when enabled. Assert true/false feature gate at heights 19/20/21/22. |
| Actions render/hitboxes (3 failing Rust tests) | `src/tui/draw_output_options.rs` | At heights 20/21, render the Actions pipeline row directly at offset 18, without its two decorative predecessor rows. At height >=22 retain the separator, heading, and Actions row at offset 20. A single row-selection helper determines drawing and hit registration. Existing regression tests now verify 20/21/22 and short-pane refusal; pill hit tests cover both new controls. |
| Legacy manifest expectation (1 failing Rust test) | `tests/chunk_2_1_2_manifest_publication.rs` | A pre-existing manifest remains byte-for-byte unchanged on incremental overwrite when `write_manifest=false`. Assert no new manifest path and no whole-album backup, rather than expecting the old manifest to disappear. |
| Two unused-import compiler warnings (no failing tests) | `src/convert/pipeline/stages.rs` | Remove the production-only unused `SsrcPdfType` import and the six unused manifest helpers/types in the `chunk_2_1_3_postprocessing_gate_and_phase_tests` module. Fully qualify the single `SsrcPdfType` use inside `protected_ssrc_runtime_tests` so test compilation never depends on that removed parent import. Retain the module's three actually used manifest imports. |

No format, DSP, publication, audio execution, runtime qualification policy, or dependency change is part of this patch. `scripts/requalify_reference_r20.sh` remains unchanged and operates on the *current checkout*, despite its historical R20 name.

## Critical qualification boundary

**Reference is NOT RELEASE-QUALIFIED from this superseding R21 archive as delivered.** Both the `#[cfg(test)]` fixture correction and the import cleanup modify `src/convert/pipeline/stages.rs`, changing its full-file SHA-256; the file is in `tonepoet-pipeline/qualification/dsd_reference_common_v18_source_lock.json`. All four installed R20 qualification artifacts remain unchanged on purpose. Do **not** modify only the JSON hashes, waive the freshness gate, or use an unqualified Reference override. Requalification on the configured real-tool build host is required even though the changed lines are test-only.

On the qualified host, enter the project's pinned Nix environment and run focused checks, then the **existing, unmodified** real-tool Reference runner with a genuine SACD ISO:

```bash
nix develop --extra-experimental-features 'nix-command flakes'
cargo test --lib issue_65_reference_execution_evidence_does_not_force_unrequested_manifest
cargo test --lib output_options_field_cycle
cargo test --lib maximized_actions_row
cargo test --lib wrapped_output_options_draw_populates_button_map_for_actions_row
cargo test --test chunk_2_1_2_manifest_publication
# Check compiler output to confirm both unused-import warnings are gone.
unset TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE
./scripts/requalify_reference_r20.sh /absolute/path/to/real-SACD.iso
```

The runner regenerates and checks all four sidecars, installs them with rollback protection, runs `reference_qualification_freshness`, the targeted and workspace tests, workspace/release builds, both black-box smokes (including the 1-second ReplayGain fixture), and the positive qualified SACD conversion. After it succeeds, perform the separate **negative wrong-source Reference-refusal smoke** described in `R20_REFERENCE_REQUALIFICATION_HANDOFF_2026-10-09.md`, in a disposable checkout. Qualification and runtime smokes are not proven by passing a static audit.

## What was verified in the delivery environment

- Corrected file set compared with the original R20C2 source archive (five existing files only), and with the earlier R21 delivery (only `stages.rs` changes among source files).
- Checked the import removals against exact prior occurrences, and moved the one test-only `SsrcPdfType` use to a fully qualified path; compiler verification still required.
- `bash -n scripts/smoke_r20_album_publication.sh scripts/requalify_reference_r20.sh`: passed.
- Exercised the smoke script's embedded Python WAV generator: 3/3 output files are mono 44.1-kHz, 44,100-frame, 1.000-s WAV files.
- SHA-256 of all Reference-locked files compared with installed v18 lock: **only** `src/convert/pipeline/stages.rs` now differs (as expected).
- Full corrective patch and incremental superseding patch reproduction, and artifact checksum verification, are recorded in the delivery-level verification report.

**Not run here:** `cargo` tests, compiling/linking, real-tool/real-audio conversion, full workspace gate, or Reference requalification. Rust, Cargo, and Nix are absent in this sandbox. The 7 Rust fixture failures are corrected at source level, not claimed to have passed execution.

## Reproducible handoff

The full-source archive contains the repository snapshot, this handoff, the **superseding** source brief, and `RESUME_R21.md`, but no `.git` directory or build cache. The full five-file unified patch applies against the original R20C2 input archive. A separate small incremental patch applies only against the preceding R21 corrective full-source release. The external SHA-256 manifest and verification report pin the deliverables. Do not mix patches with a different revision.
