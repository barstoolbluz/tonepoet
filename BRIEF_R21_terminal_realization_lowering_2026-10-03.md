# Brief R21 — the two targets cleared; ten tests that were passing now fail

Date: 2026-10-03
Base: the supplied `tonepoet-src.tar.gz` is the `auto-deemphasis` branch with
the R20 fix applied unchanged. It compiles with zero warnings; no edits were
needed.

The gate went from 7291 passed / 4 failed to 7282 passed / 13 failed.

## Cleared

```
semantic_plan::tests::one_track_album_keeps_group_binding_with_track_equivalent_policy_and_terminal_proof
tui::context_menu::tests::context_menu_convert_last_used_audio_directory_commits_after_async_expansion
```

## Expected

`installed_reference_qualification_binds_the_current_locked_sources`.
`semantic_plan.rs` is Reference-locked. Requalification runs on this host.

## Pass in isolation

```
concurrency::tests::journal_operation_final_logical_owner_unlocks_accidental_fork_copy_immediately
concurrency::tests::same_process_journal_coholder_shares_deliberate_export_authority
```

## The ten

Each was passing before the R20 fix. Each carries the same error:

```
InvalidSettings { field: "terminal_realization",
  reason: "selected direct terminal has 0 matching lowered PCM operations instead of one" }
```

```
tonepoet-pipeline/tests/planning.rs
  default_dsd_to_pcm_matches_explicit_strict_general_plan
  directional_dsd_sinc_parameters_are_bound_end_to_end
  dsd_fixed_ultra_rate_mapping_normalizes_dormant_resample_quality
  dsd_lowpass_paths_all_use_sox_ultra_rate_flag
  dsd_sinc_quantization_matches_retained_argv_and_aliasing_refuses

src/convert/pipeline/plan_bridge.rs
  sacd_flac_plan_has_no_ffmpeg_map_metadata_or_source_md5_from_materialized_dsf
  staged_dsf_and_dff_flac_plans_still_use_ffmpeg_source_metadata_transfer

src/convert/pipeline/stages.rs
  nine_dff_metadata_sidecar_album_drives_real_conversion_naming_and_flac_tags
  untaggable_dts_sidecar_cue_full_pipeline_embeds_authoritative_tags

src/convert/pipeline/track_executor.rs
  sacd_track_plan_reports_no_planner_metadata_satisfaction_for_source_tag_policy
```

## The outcome we want

The gate is green, apart from the Reference requalification which runs here.

## State

7282 passed / 13 failed across 63 targets, zero warnings.
`tonepoet-true-peak` is gated separately and was not run; CLAUDE.md records it
at 160/0.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The root `tonepoet` crate
peaks at 6.24 GB in one `rustc` and will OOM under a 4 GB ceiling; write those
changes uncompiled and say so.
