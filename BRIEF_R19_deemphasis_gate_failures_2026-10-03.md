# Brief R19 — R18 compiles, nine tests fail

Date: 2026-10-03
Base: the supplied `tonepoet-src.tar.gz` is the `auto-deemphasis` branch with
the R18 snapshot applied and one import added (below).

R18 applied cleanly: 18 files, +2,753 / −140. After one fix it compiles with
zero warnings. The gate is 7267 passed / 10 failed.

## The one change we made

`src/tui/app.rs`, the test
`mixed_batch_source_policy_uses_eligible_member_cd_facts_not_representative`
used `SOURCE_SAMPLE_RATE_SENTINEL` without importing it. Added
`use super::SOURCE_SAMPLE_RATE_SENTINEL;` at the top of that test, matching its
siblings. Nothing else was touched.

## Expected, not a defect

`installed_reference_qualification_binds_the_current_locked_sources` fails
because R18 changes four Reference-locked files: `plan_bridge.rs`, `stages.rs`,
`fingerprint.rs`, `semantic_plan.rs`. Requalification runs on this host.

## Not R18

`concurrency::tests::same_process_journal_coholder_shares_deliberate_export_authority`
passes in isolation. Known coordination flake.

## The nine failures

All deterministic: each fails in isolation as well as in the suite.

### Three in `tonepoet-pipeline/src/semantic_plan.rs`

```
cd_deemphasis_forced_ssrc_flac_24_882_preserves_one_resampler_and_package_owner
  :10114  package-only continuation must not resample
          left: Some(88200)   right: None

cd_deemphasis_ssrc_wavpack_int24_preserves_sox_package_only_owner
  :10157  left: Some(88200)   right: None

cd_deemphasis_ssrc_float64_split_terminal_keeps_one_rate_change
  :10223  downstream terminal must not resample
          left: Some(176400)  right: None
```

### Two in `src/tui/probe.rs`

```
convert_deemphasis_evidence_tests::convert_authority_accepts_only_exact_key_one_or_yes
  :8737   panicked with: 1

convert_deemphasis_evidence_tests::convert_catalog_advisory_requires_actual_exact_catalog_tag
  :8753   left: None   right: Some("35DP 150")
```

### Two in `src/convert/pipeline/stages.rs`

```
cue_real_output_matrix_tests::cue_matrix_validates_real_outputs_when_external_tools_are_available
cue_real_output_matrix_tests::single_file_custom_tags_survive_real_writer_matrix_and_converge

  both: cue_to_opus first metadata pass missing metadata key PRE_EMPHASIS
```

### One in `src/tui/context_menu.rs`

```
context_menu_convert_last_used_audio_directory_commits_after_async_expansion
  :5984   left: Convert   right: Browse
```

## The outcome we want

The gate is green, apart from the Reference requalification which we run here.

## State

Before R18 the gate was 7244 passed / 0 failed across 63 targets. It is now
7267 / 10. `tonepoet-true-peak` is gated separately and was not run; CLAUDE.md
records it at 160/0.

## Build capability

`cargo test -p tonepoet-pipeline` builds in seconds. The root `tonepoet` crate
peaks at 6.24 GB in one `rustc` and will OOM under a 4 GB ceiling; write those
changes uncompiled and say so.
