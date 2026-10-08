# TonePoet R18 — #53/#65/#66 corrective repository handoff

Date: 2026-10-08. Baseline: R17 complete repository archive `TonePoet_LOGGING_R17_fixtures_53_66_2026-10-08.tar(1).gz` (SHA-256 `438cb9dac69d0f45b1c22032672751dd1172d0dc1cfc78826e9c7aaddc722665`). This is a full edited repository snapshot, **not** an accepted build-host qualification.

## Scope and changes

**#65 — audio fixture:** The nine-route post-conversion inventory matrix now overwrites the staged fake FLAC payload with the existing `tests/fixtures/silence.flac`. The two qualified-Reference evidence tests use the same parseable fixture. No production ReplayGain or metadata-disposition guard was changed. The Reference mock runner still observes post-metadata FFmpeg decoded-sample verification, and a deliberately corrupted packaged sample digest must still block publication. Manifest publication is tested under explicit consent off/on; human conversion log off/on.

**#65 — fragment terminal lifecycle:** `issue_65_independent_folder_album_publish_has_no_extra_entries_with_log_on_or_off` now calls the production `finalize_report_with_binding_and_timing` after *each* real fragment publication, so the batch registry and `.tonepoet-batch` coordination state are cleaned by the existing terminal lifecycle before destination inventory. It does not delete workspace files itself. The independent inventory-helper pollution test remains intact.

**#53 — rerun detection withdrawn:** Removed `rerun.rs`, `orchestrator_rerun_gate.rs`, the two obsolete overwrite-policy variants, pre-materialization and post-planning skip gates, reference-only rerun preflight, and the album-gain post-barrier rerun work unit. The album-gain barrier releases ready albums through the ordinary scheduler. Pre/post actions, explicit collision policies (`FailIfExists`, `ReplaceWithBackup`, `AlwaysRedo`), publisher lock/backup behavior, unrelated fingerprints, requested manifest provenance, and Reference sample authority survive.

Before publication, qualified Reference execution evidence is validated independently of manifest consent by `validate_reference_execution_authority` in `manifest_builder.rs` using the same existing route and sample-continuity checks. `build_manifest_for_album` is reached only when `req.publish.write_manifest` is true; no implicit Reference manifest is constructed or published. There is no new validation framework.

The old two integration-test crates only tested the withdrawn rerun API and could no longer compile. The five manifest/path/round-trip tests were preserved in `tests/chunk_2_1_2_manifest_publication.rs`; two replacement publisher tests cover a prior matching manifest with fail-if-exists and replace-with-backup, without automatic republishing.

**#66 — independent log consent:** Unified request construction maps `write_log_file` solely to `write_conversion_log`, never to `write_json_log`; a caller supplying a complete `PipelineRequest` may still explicitly request JSON independently. The terminal human-log delivery section omits the machine-evidence field when unrequested, includes the location when written, and reports `unavailable` only when requested but absent. Idempotent terminal-section replacement is retained and tested.

## Focused build-host commands (NOT executed in this environment)

This container has no Rust compiler, Cargo or rustfmt and no real SACD conversion fixture. **Cargo compilation, all named Rust tests, full workspace validation and production-conversion acceptance remain unverified.** Execute on the configured host with Rust >= 1.93 and project dependencies:

```bash
cargo test --lib issue_65_postconversion_destination_inventory_covers_source_routes_and_log_consent
cargo test --lib issue_65_reference_execution_evidence_does_not_force_unrequested_manifest
cargo test --lib issue_65_reference_execution_still_validates_sample_identity_without_manifest
cargo test --lib issue_65_independent_folder_album_publish_has_no_extra_entries_with_log_on_or_off
cargo test --lib destination_inventory_detects_hidden_files_nested_directories_and_unrequested_evidence
cargo test --lib terminal_delivery_evidence_reflects_independent_request_and_result
cargo test --lib human_log_flag_does_not_request_machine_evidence
cargo test --lib independently_requested_machine_evidence_survives_prebuilt_request
cargo test --test chunk_2_1_2_manifest_publication
cargo test --lib album_gain
cargo test --workspace
cargo build --workspace
```

For final build-host acceptance, inspect the outcome and assertions, not merely test names: Reference success cases should actually reach post-metadata verification; corrupted sample identity should fail for Reference authority, not Lofty parsing; destination inventories should have only requested artifacts after terminal cleanup; matching prior manifests must not skip conversion; the album-gain scheduler must still resolve/dispatch; and equivalent CLI/TUI human-log requests must not emit machine JSON unless independently requested. A real qualified SACD sample cannot be substituted with these synthetic fixtures for release qualification.

## Verification already performed locally

- R17 source archive passed gzip integrity and had SHA-256 `438cb9dac69d0f45b1c22032672751dd1172d0dc1cfc78826e9c7aaddc722665`.
- `ffprobe` decoded the reused FLAC fixture header: `codec_name=flac`, `sample_rate=44100`, `bits_per_raw_sample=16`.
- Static source audit checked that removed rerun variants, modules and decision types have no surviving Rust references.
- Diff bounded to the affected pipeline builder, executor, request, publisher, scheduler, integration tests and this checkpoint. No production ReplayGain, folder batch cleanup or machine-evidence writer redesign.
- The code snapshot and review patch should be checked against the SHA-256 recorded separately at packaging.

## Resume points

Primary files: `src/convert/pipeline/stages.rs`, `src/convert/pipeline/manifest_builder.rs`, `src/convert/pipeline/unified_request.rs`, `src/convert/pipeline/track_executor.rs`, `src/convert/pipeline/types.rs`, `src/convert/pipeline/mod.rs`, `src/convert/processor.rs`, `tests/chunk_2_1_2_manifest_publication.rs`. Removed: `src/convert/pipeline/rerun.rs`, `src/convert/pipeline/orchestrator_rerun_gate.rs`, `tests/chunk_2_1_2_manifest_rerun.rs`, `tests/chunk_2_1_2_orchestrator_gate.rs`. All other baseline files retained unchanged.

If a build-host test fails, fix that concrete source error only; do not reintroduce rerun detection or broaden metadata/logging systems.
