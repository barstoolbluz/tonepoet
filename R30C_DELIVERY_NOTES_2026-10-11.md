# TonePoet R30C — test-only acceptance correction (2026-10-11)

**Status: corrected source candidate. Build-host test gate and new Reference qualification are NOT yet verified.**

## Source correction

Exactly two pre-existing test expectations changed in `src/convert/pipeline/stages.rs`:

1. `r29_album_ceiling_source_is_independent_of_automatic_gain` now expects `Ceiling: -0.10 dBTP ({expected})` for each of its provenance cases.
2. `r27_album_log_is_one_read_level_write_summary_without_reference_plumbing` now expects `  Ceiling: -0.10 dBTP (origin not recorded)\n` in its otherwise unchanged golden string.

Both previously expected `-0.1`, contrary to the R30 requirement of at least two displayed decimal places. All other assertions—including provenance, applied gain, and exclusion of internal Reference machinery—are unchanged. No production Rust line, DSP path, user interaction, dependency, runtime configuration, or qualification artifact was changed.

## Patch application / provenance

- This archive contains the **complete** R30C source tree. For a minimal update to an already applied R30 tree, apply `R30C_FROM_R30_TEST_ONLY.patch`.
- For the original supplied R29C snapshot, apply the existing historical `R30_FROM_R29C.patch` **and then** `R30C_FROM_R30_TEST_ONLY.patch`. The former is preserved unchanged as evidence for the first R30 revision; it alone does not yield R30C.
- `R30_CHANGED_SHA256SUMS.txt` and `R30_SOURCE_SHA256SUMS.txt` have been refreshed to reflect the corrected delivered tree. Prior R30 delivery notes and static QA remain historical records, superseded for R30C by this note and the new resume checkpoint.

## Verification performed in this environment

- Original R30 source archive SHA-256 verified: `4bedce957bdf6cb1b31272c5231b9d0ea39ffcc2bc998adf05ba7e8bbbab4ddb`.
- The original file contains exactly the two obsolete expected strings. The corrected source differs from its archive original at **only those two lines**; source line count is unchanged.
- The incremental patch passed `git apply --check`, applied cleanly to an independent original R30 copy, and reproduced the corrected file byte-for-byte.
- Full archived-source checksums and archive extraction/integrity are verified separately in the R30C checkpoint.

Rust compiler/Cargo, Nix and a real SACD qualification environment are not available here. **No compiled test has been represented as passing.**

## Acceptance required on a configured build host

```bash
cargo test --locked --lib r29_album_ceiling_source_is_independent_of_automatic_gain
cargo test --locked --lib r27_album_log_is_one_read_level_write_summary_without_reference_plumbing
cargo test --locked --lib r27_reference_music_gain_excludes_only_internal_protection_without_exposing_it
cargo test --locked --lib r30_
```

Then run `./R30_ACCEPTANCE.sh /absolute/path/to/real-SACD.iso`, which uses the existing `scripts/requalify_reference_r20.sh` for Reference requalification and the workspace gate. The existing installed Reference qualification sidecars are stale against the changed `stages.rs`; do **not** claim current qualification until fresh evidence passes. Observe the documented negative runtime-closure mismatch smoke separately. Do not waive any new deterministic test failure; distinguish any known staging lease-ownership contention flake on its actual evidence.

**Scope judgment:** No other production changes or added hardening are warranted by the reported defect.
