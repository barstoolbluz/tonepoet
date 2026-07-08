# Test Triage: Final 7 Failures — LAST PASS

127 → 7. Fix these and we're done.

## Pipeline Stages (5 tests)

3 tests fail at the SAME line (`stages.rs:20193`) with the SAME error:
```
"conversion log source X is not under album batch grouping root Y/source-root"
```

This is a validation assert YOU ADDED in the previous pass's `fragment_test_identity_from_batch()` helper. The helper rejects test fixtures where the source path isn't under the album batch's grouping root. But the existing test fixtures use paths like `/tmp/input.7z` with album dirs like `/out/Test Artist/Test Album` — they were never designed to have source inside the album dir.

**Fix: make the test helper work with the test fixtures, not reject them.** The source path doesn't need to be under the grouping root in test fixtures — that's a production path constraint that shouldn't apply to synthetic test data. Either remove the validation assert from the helper, or adjust the 3 test fixtures so their source paths satisfy it.

Affected tests:
```
all_blocked_fragment_assembly_uses_canonical_blocked_result_label (stages.rs:20193)
  source: /tmp/input.7z, root: /out/Test Artist/Test Album/source-root

fragment_assembled_log_matches_canonical_multitrack_formatter_for_equivalent_data (stages.rs:20193)
  source: /tmp/input.7z, root: /out/Test Artist/Test Album/source-root

successful_finalization_cleans_quarantine_for_finalized_album_batch (stages.rs:20193)
  source: /tmp/.../input.flac, root: /tmp/.../Album/source-root
```

The other 2 pipeline tests:
```
cancelled_fragment_batch_assembles_partial_log_and_cleans_fragments (stages.rs:22324)
  "cancellation finalization assembles a partial forensic log"

real_plan_output_failure_publishes_fragment_and_completes_batch (stages.rs:23113)
  assertion failed: log.contains("Status: Failure")
```

These may cascade from the same identity helper issue, or may be independent.

## TUI Browse (1 test)

```
archive_staging_tag_search_falls_back_to_probe_metadata_for_synthetic_entry (browse.rs:15276)
  left: [], right: ["Disc 1/01.flac"]
```

Tag search for synthetic archive entries still returns empty. The previous fix attempt didn't resolve this.

## TUI Keybindings (1 test)

```
source_tree_has_no_app_local_file_picker_and_uses_crate (keybindings.rs:28866)
  "old in-app picker module must stay deleted"
```

This test checks the source tree for a deleted module. The previous pass didn't fix it. Check what the test actually asserts and either remove the stale module reference or update the test if the crate-backed picker is properly in use.

## Your Task

Fix all 7. The 3 pipeline failures at line 20193 are from your own code from the previous pass — the validation in `fragment_test_identity_from_batch()` is too strict for the test fixtures. Fix that first, then the remaining 5 should become clearer.
