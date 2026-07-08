# Test Triage: Final 17 Failures

Started at 127 failures, now at 17. These are the last ones.

## Group 1: Pipeline Stages — postprocessing publish/finalization (11 tests)

Most fail with `DestinationExists` on the album directory, suggesting the publish path now rejects pre-existing album dirs where it previously allowed appending. Others fail with `NotFound` on filesystem operations.

```
sequential_single_file_publishes_share_album_folder_and_append_conversion_log
  → DestinationExists(".../out/Album") (stages.rs:23381)

concurrent_single_file_publishes_share_album_folder_and_append_one_conversion_log
  → DestinationExists(".../out/Album") (stages.rs:23435)

interrupted_incremental_publish_recovery_removes_partial_track_and_restores_log_before_retry
  → DestinationExists(".../out/Album") (stages.rs:23654)

interrupted_incremental_sidecar_replacement_recovery_restores_sidecar_before_retry
  → DestinationExists(".../out/Album") (stages.rs:23798)

incremental_single_file_publish_rejects_existing_audio_file_under_fail_if_exists
  → assertion on PublishError::DestinationExists matching audio file path (stages.rs:23514)

incremental_sidecar_failure_does_not_publish_audio_file
  → assertion on PublishError::DestinationExists matching conversion.log path (stages.rs:23539)

successful_finalization_cleans_quarantine_for_finalized_album_batch
  → "quarantine for the finalized album batch is cleaned" (stages.rs:22450)

cancelled_fragment_batch_assembles_partial_log_and_cleans_fragments
  → "cancellation finalization assembles a partial forensic log" (stages.rs:22272)

terminal_failed_single_track_job_publishes_fragment_without_features_stage
  → NotFound (stages.rs:22096)

real_plan_output_failure_publishes_fragment_and_completes_batch
  → NotFound (stages.rs:23043)

album_batch_context_rejects_empty_or_zero_identity_fields
  → assertion failed: validate_album_batch_context(&invalid).is_err() (stages.rs:21326)
```

The `DestinationExists` cluster (6 tests) likely shares a single root cause: the publish function now checks for album dir existence and rejects it, where previously it allowed creating or appending into an existing dir. The `NotFound` tests (2) are probably missing directory creation in test setup. The finalization tests (2) and the batch context validation (1) may be separate issues.

File: `src/convert/pipeline/stages.rs`

## Group 2: DVDA LPCM nibble decoding (1 test)

```
decodes_20_bit_group1_and_group2_nibbles_like_foo_input_dvda
  → left: [0, 0, 1, 0], right: [0, 160, 1, 0] (dvda_lpcm.rs:832)
```

A 20-bit LPCM nibble decoding assertion. The second byte should be 160 (0xA0) but is 0. This is a real logic bug in the nibble unpacking, not a stale test.

File: `src/convert/pipeline/dvda_lpcm.rs`

## Group 3: DVDA Materializer (3 tests)

```
realized_wav_validation_runs_for_missing_rate_even_with_stream_label
  → assertion failed: track_needs_realized_wav_audio_facts_validation(&track) (materializer_dvda.rs:4434)

seven_disc_fixture_corpus_rejects_the_three_known_cppm_discs
  → "known CPPM fixture should be blocked after structure materialization" but hawks_and_doves succeeded (materializer_dvda_fixture_tests.rs:789)

seven_disc_fixture_corpus_materializes_structure_with_expected_track_counts
  → "known CPPM fixture materialized successfully" for hawks_and_doves (materializer_dvda_fixture_tests.rs:735)
```

The two fixture corpus tests: `hawks_and_doves` was previously blocked as CPPM-protected but now materializes successfully. Either the CPPM detection changed (and the test expectations need updating) or the detection regressed (and the code needs fixing).

The `realized_wav_validation` test: the validation function returns false where the test expects true. The `track_needs_realized_wav_audio_facts_validation` logic likely changed.

Files: `src/convert/pipeline/materializer_dvda.rs`, `src/convert/pipeline/materializer_dvda_fixture_tests.rs`

## Group 4: TUI Browse (2 tests)

```
tag_only_search_keeps_disc_directories_navigable_by_filename
  → "directories, including disc directories, must remain filename-searchable for navigation" (browse.rs:16695)

archive_staging_tag_search_falls_back_to_probe_metadata_for_synthetic_entry
  → left: [], right: ["Disc 1/01.flac"] (browse.rs:15231)
```

The disc directory search test asserts that directories remain findable by filename even when tag-only search is active. The archive staging test expects a synthetic entry to be found via probe metadata fallback but gets no results.

File: `src/tui/browse.rs`

## Your Task

Fix all 17. The source files in this bundle are the current state. For each failure, determine whether the test expectation is stale (update the test) or the code has a bug (fix the code). The `DestinationExists` cluster in Group 1 is likely one root cause affecting 6 tests.
