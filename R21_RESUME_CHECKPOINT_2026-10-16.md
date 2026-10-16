# TonePoet R21 — engineering / resumption checkpoint (2026-10-16)

## Authority and inputs

- Requested change: `BRIEF_R21_dither_test_and_metadata_authority_2026-10-16.md`.
- Original supplied repository archive: `TonePoet_LOGGING_R21_dither_and_metadata_authority_2026-10-16.tar.gz`.
- Original archive SHA-256: `358c1a51f2e54bc104f07c1d876dd25cdd0430c8578e1afd56dbce9db1012cb1`.
- Supplied archive already carried two mismatches in its own `SHA256SUMS`: `CLAUDE.md` and `src/tui/rf64_metadata.rs`. Both remain byte-for-byte identical to the supplied archive in this corrective snapshot. The inventory is regenerated from the *actual* packaged bytes.
- The 24 entries in `REFERENCE_COMMON_SOURCE_PATHS` in `reference_source_lock.rs`, the SACD crate, and Reference qualification assets are **not edited**.

## Diagnosis

1. **HDCD/de-emphasis test:** the failing assertion set `deemphasis_enabled = true` without setting the user's de-emphasis override. The HDCD toggle calls `apply_format_constraints`, which calls `recompute_auto_deemphasis`; the unowned boolean therefore reverted to false and automatic TPDF reverted to None. This is a fixture/precondition error, not evidence of a new production dither bug. The test now applies CD eligibility and selects de-emphasis through the same `FormatField::Deemphasis` route used by the UI, then checks independent TPDF preservation, explicit override, and reversibility.
2. **Metadata authority:** `normalized_aggregate_metadata_target_priority` appended any omitted defaults, making exclusion impossible. It now only deduplicates explicitly listed entries. Missing configuration values retain legacy defaults through `serde(default)`; `[]` and shorter lists are intentional exclusions.
3. **Unexpected writes / refusals:** The metadata editor's direct CUE write entrypoint and Save admission now reject a carrier class omitted from the configuration. Uncovered transfer files no longer silently resolve to `individual-files` when that target is excluded, and standalone embedded CUE groups do not silently fallback either. Existing Album-view planning already resolves targets per side, stages the batch, revalidates and publishes, so no parallel replacement architecture was introduced.
4. **False 'no carrier changed' assertion:** The old dedicated-sidecar Save path checked `CueSidecarWritebackPlan.preflight_error` downstream, *after* it had sent audio-tag changes to the writer. A detected preflight refusal is now enforced synchronously **before** launching any mutation. Two single-image preflight messages now accurately say no audio file/sidecar was changed, rather than that image tags had been saved.

## Files changed relative to the supplied source snapshot

- `src/config.rs`: explicit exclusions, round-trip and default tests.
- `src/metadata_authority.rs`: authority membership predicate, resolver and omission tests.
- `src/tui/tag_interchange.rs`: tests updated for intentional omissions and empty priority.
- `src/tui/app.rs`: reproduce HDCD-Off/CD de-emphasis TPDF with actual user policy, no production dither changes.
- `src/tui/keybindings.rs`: admission/preflight fixes and four regression tests targeting real FLAC+CUE fixtures.
- `SHA256SUMS`: rebuilt from the delivered tree (source copy was already stale), plus this checkpoint.

## Added regression tests

- `hdcd_off_preserves_user_dither_and_independent_deemphasis_tpdf` (corrected existing regression)
- `r21_omitted_sidecar_is_never_written_during_file_first_composer_save`
- `r21_direct_sidecar_write_refuses_a_disabled_carrier_without_mutation`
- `r21_sidecar_preflight_refusal_never_writes_file_tags_first`
- `r21_uncovered_transfer_files_cannot_use_omitted_individual_target`
- `omissions_are_not_write_authorities` and additional config persistence checks.

## Verification and limitations

The delivery host **does not have `rustc`, `cargo`, `rustup`, or `rustfmt`**, and DNS resolution for `static.rust-lang.org` is unavailable. Therefore no Rust compile, Rust test suite, full integration gate, performance benchmark, or Reference qualification was run for this new snapshot. The source is an implemented corrective candidate, **not** a build-host-accepted release. Never label the new tests as passing until they execute.

Compile-free verification performed in the delivering session: source-diff review, baseline-package checksum anomaly identification, unchanged Reference-locked source-byte comparison, package-wide regenerated SHA-256 inventory verification, tarball existence/extraction verification. Results and any exceptions are recorded in `R21_VALIDATION_2026-10-16.txt`.

## Mandatory build-host handoff commands

```sh
set -eu
sha256sum -c SHA256SUMS
cargo fmt --all -- --check
cargo test --locked hdcd_off_preserves_user_dither_and_independent_deemphasis_tpdf
cargo test --locked r21_
cargo test --locked --all-targets
```

Run the repository's established Reference freshness/qualification checks without regenerating Reference certificates: `cargo test --locked --test reference_qualification_freshness`. Run the existing `rf64_` regressions as well: `cargo test --locked rf64_`.

If a new regression fails on the build host, repair the smallest diagnosed defect before handoff. For the in-field de-emphasis symptom, additionally capture the exact sequence of `dither_overridden`, `dither_override_from_preset`, `deemphasis_enabled`, eligible 16/44.1 source facts, and displayed target depth; this still needs field evidence. Also not in scope: R5 UI inspection, #75 physical SACD round trip, #73 Markdown popup.

## Resume protocol

Start from **this full packaged tree**, not from an older snapshot. Verify `SHA256SUMS`, inspect this checkpoint and the source brief, then run the listed Rust gates on a qualified build machine. Preserve all source-locked bytes and maintain config-target admission as the single authority. Avoid expanding work into architectural rewrites unless a regression demonstrates a specific failure.
