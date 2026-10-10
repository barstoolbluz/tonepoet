# TonePoet R25C — narrow corrective handoff

Date: 2026-10-10. **Baseline:** the previously delivered `TonePoet_LOGGING_R25_CORRECTIVE_2026-10-10.tar.gz` source, *not* the original user-supplied R24/R25 intake archive. The Git commit label from upstream was not available as Git metadata in this source archive. **Delivery status:** source changes and static integrity checks complete; Rust tests and Reference requalification are **not** verified here (this environment has no Rust/Cargo/Nix toolchain or real SACD ISO fixture).

## Scope and production changes

Only `src/convert/pipeline/stages.rs` differs from the delivered R25 tree. No DSP, SACD metadata-provenance, naming, manifest, rollback state machine, or other production module changed. Historical `R25_*` records are kept for provenance; the new `R25C_*` records describe the corrected tree.

1. **Ordered no-log sibling admission:** `preflight_proven_destination_obstruction` now treats dispatcher-authored, track-context-bearing ordered folder jobs as incremental append operations even when `write_conversion_log=false`. An unoccupied second filename under an already published album root is admitted. An occupied audio filename still fails under `FailIfExists`. Absent a dispatcher track context, the old conservative root refusal remains. `overwrite` and `keep-both` still bypass this FailIfExists-specific preflight.
2. **Target-specific crash/retry evidence:** A batch FAILED/ACTIVE status alone no longer authorizes an occupied audio target. Incremental publisher audio reuse now needs (i) a provably unfinished same-batch workspace, (ii) an already installed successful fragment for the **same** batch, track sort key and output target, and (iii) an identical staged coordination fragment, before comparing staged and installed audio. If the evidence is missing or invalid, the result is `DestinationExists`, regardless of audio equality. The early admission exception uses the same installed per-target success evidence. It cannot be borrowed from a sibling's failure.
3. **R23 rollback journal retained:** For a crash during per-file audio installation, admission may defer only if an existing validated incremental recovery marker specifically lists that output as `RemoveCreatedFile`. The publisher then *rolls back* the interrupted file before republishing rather than claiming a byte-identical reconversion. A journal for another file does not admit this target. Whole-root marker repair, multi-root marker repair, and existing rollback implementations were not refactored.
4. **Multi-root recovery:** A provably unfinished batch can pass preflight for occupied multi-root output only if *every* planned output has an installed success fragment for its batch/track/target. The existing multi-root publisher still requires matching staged fragments and audio for recovery, and otherwise returns `DestinationExists`.

### Evidence boundary / preexisting crash window

R25's old `out_of_album_fragment_repair_allows_retry_after_audio_visible_crash` fixture created a preexisting file and a dead batch owner but **no durable publication evidence for that file**. The modified fixture now includes the R23 durable `RemoveCreatedFile` action that a crash *during* audio installation actually leaves; it verifies that repair removes the incomplete payload and the current attempt republishes it. A separate regression refuses an identical preexisting audio file when a dead batch owner has no marker and no installed target fragment. This avoids conflating an unknown older file with a successful same-batch publication.

There remains an older protocol window **after** the per-file rollback marker has been committed/removed but **before** the out-of-album success fragment is durably installed. A hard crash in that window can leave audio with no durable target-specific attribution. R25C deliberately fails closed there; it does **not** claim to recover that unprovable file or add a new protocol to cover the window. Resolving this separately would require an explicitly accepted per-target durable evidence protocol. Do not 'repair' it by restoring broad dead-owner byte-matching.

## Regression coverage added/updated (source present, not executed)

- `r25c_ordered_two_track_batch_with_log_disabled_appends_and_refuses_reconversion`: real feature-stage hidden fragments, ordered dispatcher batch, two independent tracks into one album, no visible `conversion.log`, occupied-track redo refused by preflight.
- `r25c_failed_sibling_cannot_authorize_prior_batch_audio_reuse`: first sibling's actual publication refusal marks batch FAILED; second's identical incumbent from an earlier batch is refused before extraction and by direct publisher; original audio and human success log unchanged.
- `r25c_recovery_requires_matching_installed_track_fragment`: same-batch target-specific installed fragment permits direct publisher repair.
- `r25c_multi_root_retry_preflight_requires_every_installed_target_fragment`: batch status and only one fragment cannot excuse occupied multi-root destinations.
- `out_of_album_fragment_repair_allows_retry_after_audio_visible_crash`: now models a validated per-target R23 rollback marker and provably dead owner; marker removed and audio republished.
- `r25c_dead_owner_without_target_publication_evidence_cannot_claim_old_audio`: no installed target fragment and no journal => early and direct `DestinationExists`.

Retained unchanged: `write_conversion_log_disabled_suppresses_fragment_for_batch_jobs`, `multi_root_retry_repairs_roots_and_fragments_committed_before_batch_finalization`, existing R25 fresh collision tests, catalog provenance tests, and full R23 journal tests.

## Build-host acceptance still required

Use the actual build-host Rust/Nix environment; do not confuse the source-level integrity checks below with compilation or execution:

```bash
cargo fmt --all -- --check
cargo test --lib r25c_
cargo test --lib out_of_album_fragment_repair
cargo test --lib write_conversion_log_disabled_suppresses_fragment_for_batch_jobs
cargo test --lib multi_root_retry_repairs_roots_and_fragments_committed_before_batch_finalization
cargo test --lib r25_fail_if_exists_preflight_refuses_occupied_paths_before_extraction
cargo test --lib r24_sidecar_field_origins_follow_selected_values_without_exporting_log_markers
cargo test --workspace
cargo build --workspace
cargo build --release
./scripts/smoke_cli_convert_exit_codes.sh ./target/release/tonepoet
./scripts/smoke_r20_album_publication.sh ./target/release/tonepoet
unset TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE
./scripts/requalify_reference_r20.sh /absolute/path/to/real-SACD.iso
```

The executable real-SACD Browse → Convert → preset TUI gesture (#43) remains an independent manual acceptance prerequisite. Since `stages.rs` changed, the previously installed Reference qualification **does not bind R25C**; requalify once after final build-host fixes. Confirm the expected source-lock/freshness gate rather than adjusting the Reference lock by hand.

## Static verification and application

`R25C_CODE_CHANGES.patch` is a reviewable one-file delta **against delivered R25**. It was checked with `git diff --check`, applied into a fresh R25-baseline scratch copy, and reproduced `src/convert/pipeline/stages.rs` byte-for-byte. A whole-tree comparison against delivered R25 shows no other changes to existing production files. Verify exact hashes using `sha256sum -c R25C_CHANGED_SHA256SUMS.txt` from the extracted `tonepoet/` root. For direct patch use: `git apply R25C_CODE_CHANGES.patch` in a checkout containing delivered R25 code.

No dependency migration, new persisted state format, journal architecture, metadata redesign, or performance-sensitive ordinary conversion byte comparison was added.
