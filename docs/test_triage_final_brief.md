# Test Triage: 50 Remaining Failures (Pipeline + TUI/Disc)

## Summary

50 test failures remain after two previous triage passes that fixed 77 tests. These break into pipeline (~38) and TUI/disc (~12) groups. Full panic output for every failure is included below, grouped by root cause.

## Root Cause Groups

### Group 1: Bluray Realize — "Not a Blu-ray directory source" (4 tests)

All 4 `bluray_realize` tests fail with `TrackValidation("Not a Blu-ray directory source")`. The tests create a temp disc-root but the validation rejects it.

```
compressed_bluray_audio_success_validates_publishes_and_returns_wav — TrackValidation("...not usable by the ffmpeg bluray protocol: Not a Blu-ray directory source...")
compressed_bluray_audio_routes_to_ffmpeg — same
compressed_bluray_last_chapter_omits_duration — same
compressed_bluray_rejects_decoded_metadata_mismatch — same
```

File: `src/convert/pipeline/bluray_realize.rs` (lines 1747, 1800, 1830, 1919)

Likely cause: A Blu-ray directory validation check was added or tightened, and the test fixtures don't satisfy it (missing BDMV structure, PLAYLIST files, etc.).

### Group 2: Materializer Bluray — assertion mismatches (4 tests)

```
materializer_bluray_default_scoring_matches_browser_scoring — panic "browser default presentation" (line 2245)
materializer_bluray_lpcm_probe_failure_blocks_track_creation — left: 2, right: 1 (line 2060)
materializer_bluray_lpcm_probe_success_populates_bit_depth — left: 2, right: 1 (line 2006)
materializer_reuses_mapper_probed_compressed_bit_depth_without_reprobe — left: Some(96000), right: Some(192000) (line 2115)
```

File: `src/convert/pipeline/materializer_bluray.rs`

Likely cause: Track count, sample rate, or scoring logic changed.

### Group 3: Materializer DVDA — fixture corpus (9 tests)

4 tests fail with `golden DVD-Audio probe output has no entry for fixture .../ap_eye_in_the_sky (normalized name apeyeinthesky)`:
```
seven_disc_fixture_corpus_audio_facts_match_golden_probe_where_ifo_proves_them
seven_disc_fixture_corpus_cppm_matches_golden_probe_outcomes
seven_disc_fixture_corpus_group_counts_match_golden_probe_not_parser_model
seven_disc_fixture_corpus_track_boundaries_match_golden_probe
```

3 tests fail with `DVD-Audio disc-absolute sector reads require an ISO image`:
```
seven_disc_fixture_corpus_group_selection_matches_the_parser_model
seven_disc_fixture_corpus_rejects_the_three_known_cppm_discs
seven_disc_fixture_corpus_track_selection_filters_after_materialization
```

1 test fails with materialization io error (`No such file or directory`):
```
seven_disc_fixture_corpus_materializes_structure_with_expected_track_counts
```

1 test fails with `corpus_probe_output.json must contain exactly the seven DVD-Audio fixture entries — left: 4, right: 7`:
```
seven_disc_fixture_corpus_has_parser_independent_golden_probe_data
```

File: `src/convert/pipeline/materializer_dvda_fixture_tests.rs` (line 270, 732, 766, 805, 854, 884)

Likely cause: The `ap_eye_in_the_sky` fixture was added to the fixture directory but not to `corpus_probe_output.json`, and/or the fixture is a directory copy (not ISO) which a new validation rejects.

### Group 4: Materializer DVDA — non-corpus (3 tests)

```
samg_only_track_materializes_without_ats_title_reference — left: Some(1), right: Some(12) (line 6511)
cross_ats_stereo_identity_chain_materializes_mlp_hint_and_auto_downmix — left: None, right: Some("MLP") (line 6900)
realized_wav_validation_runs_for_missing_rate_even_with_stream_label — assertion failed: track_needs_realized_wav_audio_facts_validation(&track) (line 4366)
```

File: `src/convert/pipeline/materializer_dvda.rs`

### Group 5: DVDA LPCM (2 tests)

```
decodes_20_bit_group1_and_group2_nibbles_like_foo_input_dvda — left: [0, 0, 1, 0], right: [0, 160, 1, 0] (line 832)
lpcm_matches_foo_input_dvda_reference_vectors — libstdc++.so.6 missing (line 1049)
```

File: `src/convert/pipeline/dvda_lpcm.rs`

The `lpcm_matches` failure is an environment issue — the test spawns a binary that needs `libstdc++.so.6` which is not on `LD_LIBRARY_PATH` in the nix dev shell. Fix: add `pkgs.stdenv.cc.cc.lib` to `LD_LIBRARY_PATH` in `flake.nix` shellHook. But that's a flake change, not a Rust code change — flag it but don't block on it.

The `decodes_20_bit` failure is a real assertion mismatch in the nibble decoding logic.

### Group 6: DVDA Realize (1 test)

```
phase3_corpus_extracts_an_aob_boundary_crossing_track — "DVD-Audio Phase 3 corpus did not expose any prepared track crossing an AOB part boundary" (line 7057)
```

File: `src/convert/pipeline/dvda_realize.rs`

This test requires an external corpus directory (`TONEPOET_DVDA_PHASE3_CORPUS_DIR`) which doesn't exist. It skips individual fixtures but then panics because none were available. Should gracefully skip when no corpus exists.

### Group 7: Pipeline Stages — postprocessing (10 tests)

Multiple `chunk_2_1_3_postprocessing_gate_and_phase_tests` failures:

```
album_batch_context_rejects_empty_or_zero_identity_fields — assertion failed: validate_album_batch_context(&invalid).is_err() (line 21310)
fragment_mode_stages_only_hidden_fragment_sidecar — NotFound (line 21079)
tag_total_tracks_never_drive_fragment_completion_threshold — NotFound (line 21299)
cancelled_fragment_batch_assembles_partial_log_and_cleans_fragments — panic (line 22256)
successful_finalization_cleans_quarantine_for_finalized_album_batch — panic (line 22434)
publish_failure_blocks_and_still_writes_durable_log — assertion on AlbumOutcome::Blocked (line 19631)
publish_lock_uses_hidden_stable_file_and_removes_stale_visible_lock — panic (line 23461)
concurrent_single_file_publishes_share_album_folder_and_append_one_conversion_log — panic (line 23433)
real_plan_output_failure_publishes_fragment_and_completes_batch — NotFound (line 23027)
terminal_failed_single_track_job_publishes_fragment_without_features_stage — NotFound (line 22080)
```

File: `src/convert/pipeline/stages.rs`

Several show `NotFound` errors suggesting filesystem setup in tests isn't creating expected directories/files. Others have assertion mismatches on outcome types.

### Group 8: Pipeline Stages — conversion log (5 tests)

```
conversion_summary_shows_rate_depth_and_processing_changes — missing "Conversion: 24-bit/96kHz FLAC → 16-bit/44.1kHz FLAC (SSRC resampling, TPDF dither)" (line 17249)
dsd_source_rate_target_source_logs_planner_default_pcm_rate — missing "Conversion: DSD64 DSD → 88.2kHz FLAC" (line 17282)
pcm_to_dsd64_target_summary_uses_dsd_rate_label_not_hz — missing "Conversion: 24-bit/96kHz WAV → DSD64 DSF" (line 17327)
target_dsd_rates_are_logged_as_dsd_rate_labels — missing "Conversion: 24-bit/96kHz WAV → DSD128 DSF" (line 17304)
per_track_details_include_sizes_duration_and_command_info — missing "Source audio: 44.1kHz, 24-bit, 44100 expected samples" (line 16938)
```

File: `src/convert/pipeline/stages.rs`

Likely cause: Log format strings changed (field ordering, separator, label format). These are probably all the same root cause — a single format function was updated.

### Group 9: Pipeline Stages — naming template (1 test)

```
publish_still_rejects_existing_nested_leaf_album_dir — unexpected success (PublishedAlbum returned instead of error) (line 17857)
```

File: `src/convert/pipeline/stages.rs`

### Group 10: TUI Browse — archive search/sort (5 tests)

```
active_archive_both_search_reapplies_when_probe_metadata_arrives — left: [], right: ["track.flac"] (line 15075)
active_archive_tag_sort_reorders_when_probe_metadata_arrives — left: ["a.flac", "b.flac"], right: ["b.flac", "a.flac"] (line 15100)
archive_staging_tag_search_falls_back_to_probe_metadata_for_synthetic_entry — left: [], right: ["Disc 1/01.flac"] (line 15225)
archive_tag_cache_is_password_identity_scoped — assertion failed: tags.tag_string.contains("correct artist") (line 15318)
browse_preemphasis_checks_are_worker_side_only — "fresh browse probes must not drop read_metadata errors..." (line 14639)
```

File: `src/tui/browse.rs`

### Group 11: TUI Browse — disc navigation (1 test)

```
tag_only_search_keeps_disc_directories_navigable_by_filename — "directories must remain filename-searchable" (line 16689)
```

File: `src/tui/browse.rs`

### Group 12: TUI Keybindings (3 tests)

```
browse_double_click_descends_into_disc_directory_kinds — "there is no reactor running, must be called from the context of a Tokio 1.x runtime" (disc_browser.rs:368)
theme_gallery_enter_applies_builtin_slug_without_customizing_it — left: "catppuccin", right: "gruvbox" (keybindings.rs:6657)
source_tree_has_no_app_local_file_picker_and_uses_crate — "old in-app picker module must stay deleted" (keybindings.rs:28559)
```

The Tokio one needs `#[tokio::test]` or the called function changed to require a runtime.
The gallery test's cursor lands on "catppuccin" instead of "gruvbox" — gallery ordering or cursor initialization changed.
The file picker test checks for absence of a module that may have been re-added.

### Group 13: TUI Command (1 test)

```
bluray_toml_parser_reads_identity_tags_and_extension_fields — left: Some(String(" { vendor = \"yes\" }")), right: Some(Object {"vendor": "yes"}) (line 11127)
```

File: `src/tui/command.rs`

A TOML value is being stored as a raw string instead of parsed as a table/object.

### Group 14: TUI MusicBrainz (1 test)

```
populate_sorts_entries_with_mb_keys_in_logical_positions — assertion failed: pos("ORIGINALDATE") < pos("TRACKNUMBER") (line 2560)
```

File: `src/tui/musicbrainz.rs`

Entry sort order changed — ORIGINALDATE no longer sorts before TRACKNUMBER.

## Priority Order

1. **Groups 7, 8** (15 tests) — pipeline stages, likely 2-3 root causes (NotFound = test setup, log format = one function, batch context = one validation change)
2. **Group 3** (9 tests) — DVDA fixture corpus, likely 1 root cause (missing golden probe entry + ISO requirement)
3. **Group 10** (5 tests) — browse archive search, likely 1 root cause in probe_cache or tag lookup
4. **Groups 1, 2** (8 tests) — bluray realize/materializer
5. **Everything else** — 1-test groups

## Files in This Bundle

All source files containing failing tests are included. The pipeline files are large but compress well:
- `src/convert/pipeline/stages.rs` (~918KB)
- `src/convert/pipeline/materializer_dvda.rs` (~257KB)
- `src/convert/pipeline/materializer_bluray.rs` (~95KB)
- `src/convert/pipeline/bluray_realize.rs` (~68KB)
- `src/convert/pipeline/dvda_lpcm.rs` (~41KB)
- `src/convert/pipeline/dvda_realize.rs` (~309KB)
- `src/convert/pipeline/materializer_dvda_fixture_tests.rs` (~70KB)
- Plus all TUI/disc files from previous passes

## Your Task

Fix all 50 failures. The `lpcm_matches_foo_input_dvda_reference_vectors` test (#5) requires a `flake.nix` change (add `pkgs.stdenv.cc.cc.lib` to `LD_LIBRARY_PATH`) — note this in your output but don't attempt to fix it since the flake is not in the bundle. The `phase3_corpus` test (#6) should skip gracefully when no corpus directory exists. For everything else: fix the code or update the test, whichever is correct.
