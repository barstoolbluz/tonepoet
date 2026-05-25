# Chunk 2.1.1 sentinel validation note

See `docs/chunk2_1_1_settings_sentinel_contract.md` for the formal sentinel-suite contract used by this implementation.

The original Chunk 2.1.1 brief asks for one `PipelineSettings` sentinel where every field differs from `PipelineSettings::default()` and for that same sentinel to traverse the full runtime handoff chain.

The current `PipelineSettings::validate()` rules make one valid all-non-default object impossible:

- `target_format` defaults to `AudioFormat::Flac`, but `metadata.store_source_audio_md5 = true` is valid only for FLAC targets.
- `metadata.transfer_tags` defaults to `true`, but `metadata.store_source_audio_md5 = true` requires `metadata.transfer_tags = true`.
- `flac.verify = true` is valid only for FLAC targets.

The implemented strategy splits the contradictory requirement into executable parts:

1. `raw_single_sentinel_sets_every_field_away_from_default` constructs one named-field `PipelineSettings` object where every field differs from default. That object intentionally fails validation and exists for drift detection.
2. `SENTINEL_FIELD_INVENTORY` classifies every field as raw-drift covered, valid-propagation covered, fingerprint-covered, and linked to any named validation conflict test.
3. The valid handoff sentinel set uses two valid objects. Across that set, every field has non-default runtime coverage, and each valid object traverses the real runtime handoff tests.

This is the executable interpretation compatible with the current validator. It avoids changing production validation semantics just to create a misleading fixture object.
