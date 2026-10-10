# TonePoet logging R26 — two fixture-ownership corrections

**Source:** the supplied `TonePoet_LOGGING_R26_two_fixtures_2026-10-10.tar.gz`, itself described in the brief as R25C applied at upstream commit `9b2ca9e` with the previously discovered Rust module path corrected. This archive has no `.git` metadata, so the commit label is not independently verified here. **Scope:** test-fixture setup only, in `src/convert/pipeline/stages.rs`; no production source or persisted protocol changed.

## Root cause and correction

`ensure_conversion_log_batch_workspace_owned` deliberately rejects an unclaimed coordination workspace containing pre-existing fragment files. Both failing R25C fixtures created `.tonepoet-log-fragments` and only then requested an ownership claim. This is correctly rejected as unowned state; an output batch cannot adopt a workspace just because a fabricated fragment appears to match.

The two fixtures now register a **real current-process workspace owner before installing any successful fragment**, then mark that owned batch `FAILED` after the staged results have been installed. The `generated_current_process_test_batch_id` values remain unchanged. An owned `FAILED` workspace qualifies as an unfinished attempt under the existing production retry predicate; the target-specific fragment still has to match. Replacing the batch-ID generator or weakening the fail-closed ownership check would be both less representative and less safe.

Changed tests:

- `r25c_recovery_requires_matching_installed_track_fragment`: claim before installed track-2 success fragment; then fail the batch; retain positive early admission and direct-publisher repair of byte-identical, specifically attested audio.
- `r25c_multi_root_retry_preflight_requires_every_installed_target_fragment`: create the output root and claim before writing either disc fragment; then fail the batch; retain the positive all-roots-attested preflight and the negative preflight after removing one fragment.

**Unchanged:** `r25c_failed_sibling_cannot_authorize_prior_batch_audio_reuse`, `r25c_dead_owner_without_target_publication_evidence_cannot_claim_old_audio`, `nonempty_unowned_workspace_cannot_be_claimed`, all R23 journal logic, early admission rules, direct publisher, provenance resolution, DSP, dependency and data formats. The previously reported test reference to `album_coordination_token` already uses the working `crate::convert::pipeline::coordination_name::` path in the supplied source; no new path or import was introduced.

## Verification performed

- Compared every original regular file and symlink from the supplied archive: the **only changed existing file** is `src/convert/pipeline/stages.rs` (aside from new R26 handoff artifacts).
- Verified source order in both tests: ownership claim **before** fragment installation, owned workspace transition to `FAILED` **after** fragment installation; existing positive/negative assertions remain.
- Verified no modification to production functions or the existing unowned-workspace refusal test.
- Ran `git diff --check`, standalone patch application/reproduction, and SHA-256 file integrity checks; see `R26_STATIC_QA.txt`.

**Not executed here:** `cargo`, `rustc`, `rustfmt` and `nix` are unavailable, and network DNS is unavailable for installing them. This is a source-level correction, not a claim that the fixtures have passed on the build host.

## Build-host acceptance (ordered)

From the extracted `tonepoet/` directory:

```bash
cargo fmt --all -- --check
cargo test --lib r25c_recovery_requires_matching_installed_track_fragment
cargo test --lib r25c_multi_root_retry_preflight_requires_every_installed_target_fragment
cargo test --lib r25c_failed_sibling_cannot_authorize_prior_batch_audio_reuse
cargo test --lib nonempty_unowned_workspace_cannot_be_claimed
cargo test --lib r25c_
cargo test --lib out_of_album_fragment_repair
cargo test --lib multi_root_retry_repairs_roots_and_fragments_committed_before_batch_finalization
cargo test --workspace
cargo build --workspace
cargo build --release
./scripts/smoke_cli_convert_exit_codes.sh ./target/release/tonepoet
./scripts/smoke_r20_album_publication.sh ./target/release/tonepoet
```

The supplied brief reported **7403 passes / 3 failures**, comprising a known Reference freshness failure and these two fixture failures. Expect the two fixture failures to be resolved, but **verify**; do not report a 7405-pass gate or successful Reference qualification without executing it. Since `stages.rs` changed, run the existing Reference requalification once *after* the final source has passed the workspace gate:

```bash
unset TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE
./scripts/requalify_reference_r20.sh /absolute/path/to/real-SACD.iso
```

The manual real-SACD Browse → Convert → preset gesture (#43) remains outstanding. Do not alter the Reference lock just to eliminate the expected freshness failure.

## Distribution and provenance

`R26_CODE_CHANGES.patch` is the focused test-only delta **against the supplied R26 intake tree**, not against original R25. It can be applied via `git apply R26_CODE_CHANGES.patch` only to a checkout matching that intake. `R26_CHANGED_SHA256SUMS.txt` is the active integrity manifest; the older `R25*` and `R25C*` files are historical and their source hashes no longer describe this correction. See `R26_RESUME_CHECKPOINT_2026-10-10.md` for interruption-safe continuation.
