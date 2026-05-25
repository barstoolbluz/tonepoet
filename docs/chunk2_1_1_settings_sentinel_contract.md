# Chunk 2.1.1 settings sentinel suite contract

This file is part of the Chunk 2.1.1 deliverable. It refactors one impossible clause from the original brief without reducing the correctness invariant.

## Original conflicting requirement

The original brief required one `PipelineSettings` fixture that simultaneously satisfies all three properties:

1. every top-level and nested field differs from `PipelineSettings::default()`;
2. every value passes `PipelineSettings::validate()`;
3. that same object traverses the runtime handoff chain through `PlanRequest.settings`.

The current validation rules make those three properties mutually inconsistent.

## Executable conflict proof

The test `single_valid_all_non_default_sentinel_conflict_is_executably_documented` proves the conflict in code and names each rule:

- `metadata.store_source_audio_md5 = true` requires FLAC output (`md5_requires_flac_output`).
- `metadata.store_source_audio_md5 = true` requires `metadata.transfer_tags = true` (`md5_requires_metadata_transfer_tags`).
- `flac.verify = true` is valid only for FLAC targets (`flac_verify_requires_flac_output`).

Those rules conflict with the all-non-default object shape because `target_format` defaults to `AudioFormat::Flac` and `metadata.transfer_tags` defaults to `true`. Any one-object all-non-default sentinel must move both fields away from those defaults, which invalidates the FLAC-only MD5 and verification settings.

## Refactored acceptance model

Acceptance for this chunk uses a sentinel suite:

1. **Raw drift sentinel**
   - Constructed with named `PipelineSettings` fields.
   - Every field is set explicitly.
   - Every field differs from `PipelineSettings::default()`.
   - It may fail validation.
   - It exists for compile-time drift detection and field inventory coverage, not runtime conversion.

2. **Valid propagation sentinel set**
   - Contains one or more valid `PipelineSettings` values.
   - Each value passes `PipelineSettings::validate()`.
   - The union of the set gives every conversion-affecting field a non-default value wherever the current validator permits it.
   - Every member traverses the runtime chain:

   ```text
   ConversionOptions.pipeline_settings
   -> ConversionItem.pipeline_settings
   -> PipelineRequest.settings
   -> production convert_tracks(...) PlanRequest.settings
   ```

3. **Checked inventory**
   - `tests/settings_sentinel.rs` contains `SENTINEL_FIELD_INVENTORY`.
   - Every row names the field path and classifies raw drift coverage, valid propagation coverage, fingerprint coverage, and any named validation conflict test.
   - Tests compare this inventory to `SETTINGS_FINGERPRINT_FIELD_PATHS` and the fingerprint field count.
   - `tonepoet-pipeline/tests/settings_fingerprint.rs` keeps the recursive serde field-count check when the `serde` feature is active.

4. **Field-by-field assertions**
   - Runtime handoff checks use field-by-field equality messages, not a single whole-struct assertion.
   - Fingerprint mutation tests cover every conversion-affecting field in stable, explicit order.

This contract preserves the core invariant from the brief: no conversion-affecting `PipelineSettings` field may be silently lost, defaulted, or reinterpreted anywhere in the handoff chain.
