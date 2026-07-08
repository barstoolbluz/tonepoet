# Test Triage: 5 Settings Sentinel Failures

Previous pass fixed 1 of 6. 5 remain. Two root causes:

## Root Cause 1: `flac.write_md5` missing from sentinel inventory

The fingerprint field list (right side) contains `flac.write_md5` but the sentinel inventory (left side) does not. This field was added to `PipelineSettings` but never registered in the sentinel test inventories.

Affects 3 tests:
```
sentinel_suite_inventory_matches_fingerprint_field_list (line 527)
  → left missing "flac.write_md5"

sentinel_suite_inventory_classification_is_mechanically_checked (line 539)
  → raw drift classification mismatch for flac.compression_level (cascades from inventory mismatch)

legacy_projection_inventory_lists_every_pipeline_settings_field (line 882)
  → left missing "flac.write_md5"
```

## Root Cause 2: `flac.compression_level` sentinel doesn't set non-default value

The raw sentinel and amended sentinel both leave `flac.compression_level` at its default value, so the coverage check fails.

Affects 2 tests:
```
raw_single_sentinel_sets_every_field_away_from_default (line 582)
  → "sentinel pair leaves field at default in both cases: flac.compression_level"

amended_contract_valid_sentinel_set_covers_every_pipeline_settings_field (line 661)
  → "sentinel pair leaves field at default in both cases: flac.compression_level"
```

## Fix

1. Add `flac.write_md5` to every sentinel inventory/classification list where `flac.verify` and `flac.compression_level` appear.
2. Set `flac.compression_level` to a non-default value in both the raw and amended sentinel fixtures (e.g., if default is 5, set it to 8).

That's it — two fields, applied consistently across the sentinel inventories.
