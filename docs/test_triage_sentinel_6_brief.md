# Test Triage: 6 Settings Sentinel Failures

The previous fix added `apply_legacy_resampler_defaults()` to `unified_request.rs`, which explicitly sets `sox_resampler.*` and `soxr_resampler.*` fields. This fixed the last contract test but triggered 6 failures in `tests/settings_sentinel.rs` — a separate integration test file that enforces complete field coverage inventories.

These sentinel tests ensure that every `PipelineSettings` field is accounted for in sentinel sets, legacy projection inventories, and fingerprint field lists. The new resampler field assignments need to be registered in those inventories.

## Failures

```
sentinel_suite_inventory_matches_fingerprint_field_list (line 527)
sentinel_suite_inventory_classification_is_mechanically_checked (line 539)
raw_single_sentinel_sets_every_field_away_from_default (line 582)
amended_contract_valid_sentinel_set_covers_every_pipeline_settings_field (line 661)
legacy_projection_inventory_lists_every_pipeline_settings_field (line 882)
explicit_legacy_projection_has_behavioral_assertion_for_every_field (line 1011)
```

## Your Task

Update the sentinel inventories in `tests/settings_sentinel.rs` to include the `sox_resampler.*` and `soxr_resampler.*` fields that `apply_legacy_resampler_defaults()` now sets. The pattern should be clear from the existing entries in each inventory — add the 13 resampler fields to each list/map/match that requires it.

Do NOT remove or weaken any sentinel invariants. The tests exist to catch exactly this kind of incomplete coverage. Add the missing entries.
