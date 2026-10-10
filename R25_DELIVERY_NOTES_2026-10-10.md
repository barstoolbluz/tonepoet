# TonePoet R25 — implementation and handoff

Date: 2026-10-10. Baseline: the user-supplied `TonePoet_LOGGING_R25_provenance_field_and_reconvert_removal_2026-10-10.tar.gz`, reported to contain committed R24 (`cac32f0`); the archive itself has no Git metadata, so that commit identity is not independently attested here.

## Delivery status

**Source corrective is packaged; compiled acceptance and Reference qualification are NOT established.** This execution environment has no `cargo`, `rustc`, `rustfmt`, or Nix and could not run Rust tests, formatting checks, real conversion, or the Reference requalification runner. Do not promote this checkout to the Reference release until the build-host gates pass.

Only two existing source files differ from the supplied archive:

- `src/convert/pipeline/materializer_sacd.rs`
- `src/convert/pipeline/stages.rs`

The rest of the original source tree is preserved. Additional top-level `R25_*` files and the R25 brief constitute delivery documentation, not production changes. `R25_CODE_CHANGES.patch` is a clean Git-format delta against the *unmodified* supplied source.

## Corrective behavior

1. **Truthful SACD catalog provenance.** The human `conversion.log` catalog selector and SACD origin labeling now use the same selected key/value. Album-TOC versus disc-TOC fallback, XML sidecar aliases/overrides, blank values, and non-export of log-only metadata markers are covered by tests. The R24 failing regression's TOC album catalog now carries `SACD disc TOC` origin.
2. **No success-by-identical-reconversion.** New `FailIfExists` publishes do not compare fresh audio against incumbents to turn a collision into success. Independent-track and multi-root paths now treat an occupied target as `DestinationExists`, regardless of equal bytes. Explicit overwrite and keep-both bypass this new preflight and retain their original semantics.
3. **Early destination refusal.** After output naming/planning but before the audio extraction/encode phase, existing output files or whole-album roots (including dangling symlinks) are refused under `FailIfExists`. A distinct independent-track sibling can still append into an existing album folder. A known recovery marker or a proven unfinished same-batch workspace defers to the locked transaction recovery owner; publisher-side collision checks remain authoritative.
4. **Preserved interrupted publish.** R23 incremental and multi-root rollback journals are unchanged. Byte comparisons remain only in same-batch recovery/finalization, not generic fresh collision handling. Recovery identity requires a matching batch with no completion marker and either an explicitly failed workspace or a workspace taken over from a *provably dead* local owner. A freshly active batch is not a retry. Missing or conflicting staged fragments/payloads still fail closed.
5. **Human error wording.** The two `PlanOutputs` failure handlers describe the result as `output admission failed`, rather than calling a file collision a concurrency fault.

`matching_prior_manifest_does_not_suppress_incremental_overwrite` is intentionally retained: it protects **explicit overwrite**, not the withdrawn `FailIfExists` successful-reconversion behavior.

## Focused tests included (not executed here)

- `r24_sidecar_field_origins_follow_selected_values_without_exporting_log_markers` — previously failing baseline test.
- `r25_catalog_origin_follows_the_value_printed_by_the_conversion_log` — both TOC keys, sidecar selection and override, missing values and metadata isolation.
- `r25_fresh_fragment_conversion_with_identical_audio_is_refused` — newly active batch cannot accept incumbent audio.
- `r25_fresh_multi_root_with_identical_audio_is_not_a_publish_retry` — new batch cannot impersonate multi-root recovery.
- `r25_publish_retry_requires_unfinished_prior_attempt_not_fresh_active_state` — fresh active/handled failure/dead owner takeover/completion and batch identity distinctions.
- `r25_fail_if_exists_preflight_refuses_occupied_paths_before_extraction` — file/root collision, dangling-symlink occupation, independent sibling append, multi-root collision and overwrite preservation.
- Existing interrupted same-batch fragment repair and multi-root finalization tests now provide explicit failed or dead-owner evidence rather than accepting arbitrary identical outputs.

## Build-host acceptance (required before hand-off to production)

```bash
cargo fmt --all -- --check
cargo test --lib r24_sidecar_field_origins_follow_selected_values_without_exporting_log_markers
cargo test --lib r25_
cargo test --lib out_of_album_fragment_repair
cargo test --lib multi_root_retry_repairs_roots_and_fragments_committed_before_batch_finalization
cargo test --workspace
cargo build --workspace
cargo build --release
```

Run existing CLI album-publication smokes on the compiled binary where tools and fixtures are available:

```bash
./scripts/smoke_cli_convert_exit_codes.sh ./target/release/tonepoet
./scripts/smoke_r20_album_publication.sh ./target/release/tonepoet
```

The changed `stages.rs` is covered by the installed Reference source lock. Only **after** making and verifying all final source changes, execute the established gate on a properly equipped host, with a real SACD ISO:

```bash
unset TONEPOET_DSD_REFERENCE_UNQUALIFIED_OVERRIDE
./scripts/requalify_reference_r20.sh /absolute/path/to/real-SACD.iso
```

That runner requalifies the actual source, regenerates four sidecars, executes the freshness gate and full suite, and attempts a real Reference conversion; it may also require current qualified external tools. Preserve its output and perform the wrong-source refusal smoke in a separate scratch checkout, as prescribed by `R20_REFERENCE_REQUALIFICATION_HANDOFF_2026-10-09.md`.

**Still outside automated scope:** R24/R25 inherited issue #43, the live Browse → Convert → preset gesture against a real SACD ISO inside the TUI. It remains an acceptance prerequisite and must be exercised manually on the target build host.

## Static checks performed

- Full comparison against a separately extracted original source bundle: only the two stated `.rs` files changed.
- `git diff --check` passed (no whitespace errors).
- Git patch applied cleanly against unchanged original files and reproduced both modified files byte-for-byte.
- Source-level regression anchor/invariant checks passed (see `R25_STATIC_QA.txt`). These **do not** substitute for compiled tests.

## Scope decisions

No dependency migration, broad logging redesign, manifest refactor, DSP changes, or generated qualification sidecar editing. The existing R24 12 dB Reference fixed-term gain decision is unchanged. No automatic reruns or byte-identical conversion cache is introduced.
