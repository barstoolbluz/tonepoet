# Test Failure Triage: 127 Failures Across Multiple Subsystems

## Context

The tonepoet codebase has accumulated 127 test failures across multiple subsystems over the last 2-3 weeks of active development. `cargo check` passes cleanly (zero warnings), but `cargo test` fails. The failures span:

- **Pipeline/convert** (~35 failures): bluray_realize, dvda_lpcm, dvda_realize, materializer_bluray, materializer_dvda, stages
- **TUI browse** (~10 failures): browse_perf_followup, disc_directory_navigation, archive search/sort tests
- **TUI keybindings** (~25 failures): inline_edit_behavior, progress_dialog_theme, artwork_file_picker
- **TUI theme/theme_builder** (~25 failures): theme_builder_file_tests, export/import, preview, derived scroll
- **TUI disc_browser/draw** (~5 failures): stream_summary, disc_folder rendering, theme_render
- **Other TUI** (~10 failures): command, context_menu, draw_overlays, inline_edit, musicbrainz, text_input
- **Disc backends** (~7 failures): bluray_backend_libbluray, bluray_utils, dvdv_utils

## Full Failure List

```
convert::pipeline::bluray_realize::tests::compressed_bluray_audio_routes_to_ffmpeg
convert::pipeline::bluray_realize::tests::compressed_bluray_audio_success_validates_publishes_and_returns_wav
convert::pipeline::bluray_realize::tests::compressed_bluray_last_chapter_omits_duration
convert::pipeline::bluray_realize::tests::compressed_bluray_rejects_decoded_metadata_mismatch
convert::pipeline::dvda_lpcm::tests::decodes_20_bit_group1_and_group2_nibbles_like_foo_input_dvda
convert::pipeline::dvda_lpcm::tests::lpcm_matches_foo_input_dvda_reference_vectors
convert::pipeline::dvda_realize::tests::phase3_corpus_extracts_an_aob_boundary_crossing_track
convert::pipeline::materializer_bluray::tests::materializer_bluray_default_scoring_matches_browser_scoring
convert::pipeline::materializer_bluray::tests::materializer_bluray_lpcm_probe_failure_blocks_track_creation
convert::pipeline::materializer_bluray::tests::materializer_bluray_lpcm_probe_success_populates_bit_depth
convert::pipeline::materializer_bluray::tests::materializer_reuses_mapper_probed_compressed_bit_depth_without_reprobe
convert::pipeline::materializer_dvda::fixture_corpus_tests::seven_disc_fixture_corpus_audio_facts_match_golden_probe_where_ifo_proves_them
convert::pipeline::materializer_dvda::fixture_corpus_tests::seven_disc_fixture_corpus_cppm_matches_golden_probe_outcomes
convert::pipeline::materializer_dvda::fixture_corpus_tests::seven_disc_fixture_corpus_group_counts_match_golden_probe_not_parser_model
convert::pipeline::materializer_dvda::fixture_corpus_tests::seven_disc_fixture_corpus_group_selection_matches_the_parser_model
convert::pipeline::materializer_dvda::fixture_corpus_tests::seven_disc_fixture_corpus_has_parser_independent_golden_probe_data
convert::pipeline::materializer_dvda::fixture_corpus_tests::seven_disc_fixture_corpus_materializes_structure_with_expected_track_counts
convert::pipeline::materializer_dvda::fixture_corpus_tests::seven_disc_fixture_corpus_rejects_the_three_known_cppm_discs
convert::pipeline::materializer_dvda::fixture_corpus_tests::seven_disc_fixture_corpus_track_boundaries_match_golden_probe
convert::pipeline::materializer_dvda::fixture_corpus_tests::seven_disc_fixture_corpus_track_selection_filters_after_materialization
convert::pipeline::materializer_dvda::tests::cross_ats_stereo_identity_chain_materializes_mlp_hint_and_auto_downmix
convert::pipeline::materializer_dvda::tests::realized_wav_validation_runs_for_missing_rate_even_with_stream_label
convert::pipeline::materializer_dvda::tests::samg_only_track_materializes_without_ats_title_reference
convert::pipeline::stages::chunk_2_1_3_postprocessing_gate_and_phase_tests::album_batch_context_rejects_empty_or_zero_identity_fields
convert::pipeline::stages::chunk_2_1_3_postprocessing_gate_and_phase_tests::cancelled_fragment_batch_assembles_partial_log_and_cleans_fragments
convert::pipeline::stages::chunk_2_1_3_postprocessing_gate_and_phase_tests::concurrent_single_file_publishes_share_album_folder_and_append_one_conversion_log
convert::pipeline::stages::chunk_2_1_3_postprocessing_gate_and_phase_tests::fragment_mode_stages_only_hidden_fragment_sidecar
convert::pipeline::stages::chunk_2_1_3_postprocessing_gate_and_phase_tests::publish_failure_blocks_and_still_writes_durable_log
convert::pipeline::stages::chunk_2_1_3_postprocessing_gate_and_phase_tests::publish_lock_uses_hidden_stable_file_and_removes_stale_visible_lock
convert::pipeline::stages::chunk_2_1_3_postprocessing_gate_and_phase_tests::real_plan_output_failure_publishes_fragment_and_completes_batch
convert::pipeline::stages::chunk_2_1_3_postprocessing_gate_and_phase_tests::successful_finalization_cleans_quarantine_for_finalized_album_batch
convert::pipeline::stages::chunk_2_1_3_postprocessing_gate_and_phase_tests::tag_total_tracks_never_drive_fragment_completion_threshold
convert::pipeline::stages::chunk_2_1_3_postprocessing_gate_and_phase_tests::terminal_failed_single_track_job_publishes_fragment_without_features_stage
convert::pipeline::stages::conversion_log_tests::conversion_summary_shows_rate_depth_and_processing_changes
convert::pipeline::stages::conversion_log_tests::dsd_source_rate_target_source_logs_planner_default_pcm_rate
convert::pipeline::stages::conversion_log_tests::pcm_to_dsd64_target_summary_uses_dsd_rate_label_not_hz
convert::pipeline::stages::conversion_log_tests::per_track_details_include_sizes_duration_and_command_info
convert::pipeline::stages::conversion_log_tests::target_dsd_rates_are_logged_as_dsd_rate_labels
convert::pipeline::stages::naming_template_tests::publish_still_rejects_existing_nested_leaf_album_dir
disc::bluray_backend_libbluray::tests::lpcm_probe_reports_incomplete_subheader_for_started_pid
disc::bluray_backend_libbluray::tests::lpcm_probe_reports_invalid_lpcm_header_for_reserved_codes
disc::bluray_backend_libbluray::tests::title_info_completion_frees_successful_pointer_when_event_follows
disc::bluray_utils::tests::browse_sidecar_overlay_rejects_fingerprinted_sidecar_when_current_fingerprint_missing
disc::bluray_utils::tests::reports_parser_reason_for_incomplete_lpcm_subheader
disc::dvdv_utils::tests::probe_restores_iso_reader_position_after_miss
disc::dvdv_utils::tests::probe_restores_iso_reader_position_after_success
tui::browse::browse_perf_followup_v10_tests::stale_active_recursive_dir_stats_are_cancelled_before_queueing_current
tui::browse::disc_directory_navigation_tests::tag_only_search_keeps_disc_directories_navigable_by_filename
tui::browse::tests::active_archive_both_search_reapplies_when_probe_metadata_arrives
tui::browse::tests::active_archive_tag_sort_reorders_when_probe_metadata_arrives
tui::browse::tests::archive_both_search_matches_filename_or_archive_metadata
tui::browse::tests::archive_staging_tag_search_falls_back_to_probe_metadata_for_synthetic_entry
tui::browse::tests::archive_tag_cache_is_password_identity_scoped
tui::browse::tests::archive_tags_search_uses_probe_cache_metadata
tui::browse::tests::browse_preemphasis_checks_are_worker_side_only
tui::browse::tests::probe_current_stops_when_selected_file_is_unstatable
tui::command::bluray_sidecar_tests::bluray_custom_field_deletions_remove_stale_toml_keys
tui::command::bluray_sidecar_tests::bluray_toml_parser_reads_identity_tags_and_extension_fields
tui::command::bulk_guard_behavior_tests::bulk_guard_threshold_opens_confirmation_only_above_threshold
tui::command::execute_queue_state_consistency_tests::execute_queue_does_not_publish_cue_metadata_before_successful_source_mode_update
tui::context_menu::tests::convert_stream_submenu_excludes_bluray_presentations_until_source_options_exist
tui::disc_browser::disc_stream_summary_tests::stream_summary_sorts_by_codec_channels_depth_rate_and_caps_limit
tui::draw::theme_render_tests::appearance_pane_renders_mockup_content_without_old_clutter
tui::draw::theme_render_tests::appearance_renderer_uses_injected_cached_theme_library
tui::draw::theme_render_tests::appearance_theme_name_clips_before_palette_ribbon
tui::draw::theme_render_tests::draw_ui_uses_app_theme_on_the_next_frame
tui::draw::theme_render_tests::wizard_fallback_uses_app_theme
tui::draw_browse::folder_classification_info_pane_tests::classified_disc_folder_renders_existing_disc_summary_streams_and_cap
tui::draw_overlays::tests::editor_title_sacd_mch_read_only
tui::draw_overlays::tests::editor_title_sacd_stereo_multitrack
tui::draw_overlays::tests::editor_title_single_track_sacd_shows_area
tui::inline_edit::tests::embedded_cursor_renderer_reuses_scrolled_text_input_view
tui::keybindings::artwork_file_picker_handoff_tests::source_tree_has_no_app_local_file_picker_and_uses_crate
tui::keybindings::inline_edit_behavior_tests::browse_double_click_descends_into_disc_directory_kinds
tui::keybindings::inline_edit_behavior_tests::browse_double_click_regular_file_activates_into_convert_instead_of_toggling_selection
tui::keybindings::inline_edit_behavior_tests::browse_drag_previews_and_commits_range_selection
tui::keybindings::inline_edit_behavior_tests::browse_enter_key_descends_into_disc_directory_kinds
tui::keybindings::inline_edit_behavior_tests::browse_gutter_click_toggles_without_moving_cursor
tui::keybindings::inline_edit_behavior_tests::browse_info_edit_tags_button_uses_bulk_guard
tui::keybindings::inline_edit_behavior_tests::browse_right_key_descends_into_disc_directory_kinds
tui::keybindings::inline_edit_behavior_tests::convert_metadata_click_starts_inline_edit_without_textedit_overlay
tui::keybindings::inline_edit_behavior_tests::convert_metadata_ctrl_e_opens_full_editor_escape_hatch_without_inline_editing
tui::keybindings::inline_edit_behavior_tests::convert_metadata_field_double_click_commits_inline_edit_then_opens_overlay_path
tui::keybindings::inline_edit_behavior_tests::convert_metadata_inline_escape_cancels_without_mutating_value
tui::keybindings::inline_edit_behavior_tests::convert_metadata_inline_mouse_blur_commits_before_focus_change
tui::keybindings::inline_edit_behavior_tests::convert_metadata_inline_printable_key_starts_edit_and_enter_commits
tui::keybindings::inline_edit_behavior_tests::output_inline_escape_cancels_without_mutating_value
tui::keybindings::inline_edit_behavior_tests::output_inline_mouse_blur_commits_before_focus_change
tui::keybindings::inline_edit_behavior_tests::output_inline_printable_key_starts_edit_and_enter_commits
tui::keybindings::progress_dialog_theme_tests::complete_theme_builder_action_applies_dirty_accent_assignment_to_runtime_theme
tui::keybindings::progress_dialog_theme_tests::complete_theme_builder_action_handles_apply_preset_slug
tui::keybindings::progress_dialog_theme_tests::complete_theme_builder_action_keeps_builder_open_when_pending_hex_is_invalid_on_apply
tui::keybindings::progress_dialog_theme_tests::config_b_opens_direct_apply_theme_gallery
tui::keybindings::progress_dialog_theme_tests::config_m_toggles_between_dark_and_light_theme_variants
tui::keybindings::progress_dialog_theme_tests::config_screen_theme_key_opens_apply_dialog_then_marks_frame_dirty_on_apply
tui::keybindings::progress_dialog_theme_tests::config_theme_keys_only_open_apply_dialog_when_appearance_pane_has_focus
tui::keybindings::progress_dialog_theme_tests::performance_config_keys_update_archive_listing_settings
tui::keybindings::progress_dialog_theme_tests::theme_gallery_enter_applies_builtin_slug_without_customizing_it
tui::musicbrainz::tests::populate_editor_from_mb_single_image_per_track_titles_artists_isrc
tui::musicbrainz::tests::populate_sorts_entries_with_mb_keys_in_logical_positions
tui::musicbrainz::tests::populate_supplemental_writes_isrc_catalog_and_mb_only_fields
tui::text_input::tests::ctrl_a_moves_to_home
tui::text_input::tests::ctrl_shift_letter_uses_lowercase_binding
tui::text_input::tests::unknown_ctrl_letter_is_ignored
tui::theme::theme_builder_file_tests::canonical_custom_theme_path_requires_slug_filename_match
tui::theme::theme_builder_file_tests::custom_theme_file_round_trips_roles_accents_swatches_and_locks
tui::theme::theme_builder_file_tests::custom_theme_files_with_builtin_slug_are_skipped_and_not_saved
tui::theme::theme_builder_file_tests::delete_custom_theme_file_removes_customs_but_rejects_builtins
tui::theme::theme_builder_file_tests::save_theme_file_rejects_unsafe_slug_before_building_a_path
tui::theme::theme_builder_file_tests::symbolic_save_does_not_infer_duplicate_color_bindings
tui::theme::theme_builder_file_tests::symbolic_swatch_bindings_survive_roundtrip_and_drive_bound_slots
tui::theme::theme_builder_file_tests::theme_and_override_persistence_use_final_files_without_leftover_temps
tui::theme::theme_builder_file_tests::unique_custom_slug_can_exclude_destination_file_being_overwritten
tui::theme::theme_builder_file_tests::unique_custom_slug_checks_internal_slugs_from_noncanonical_files
tui::theme_builder::tests::derived_keyboard_scroll_uses_rendered_visible_rows
tui::theme_builder::tests::export_dialog_rendered_confirm_hitbox_writes_theme_file
tui::theme_builder::tests::export_flushes_pending_hex_edit_before_serializing
tui::theme_builder::tests::export_to_canonical_custom_path_marks_builder_clean_and_saved
tui::theme_builder::tests::export_to_noncanonical_custom_dir_file_does_not_mark_builder_clean_or_saved
tui::theme_builder::tests::import_dialog_rendered_confirm_hitbox_imports_theme_file
tui::theme_builder::tests::inline_swatch_naming_records_actual_name_hitbox_only_while_active
tui::theme_builder::tests::more_menu_duplicate_creates_visible_collision_free_copy
tui::theme_builder::tests::more_menu_export_opens_dialog_and_writes_theme_file
tui::theme_builder::tests::more_menu_import_opens_dialog_and_imports_collision_free_theme
tui::theme_builder::tests::preview_lines_expand_when_card_height_has_room
tui::theme_builder::tests::repeated_export_to_same_noncanonical_path_is_slug_idempotent
```

## Scope: What's In This Bundle

This bundle contains the source files for the **TUI, disc, and db** subsystems (~90 of the 127 failures). The following test groups are included and should be fixed:

- `tui::*` — browse, keybindings, theme, theme_builder, draw, draw_browse, draw_overlays, command, context_menu, disc_browser, inline_edit, musicbrainz, text_input
- `disc::*` — bluray_backend_libbluray, bluray_utils, dvdv_utils
- `src/db.rs` — DirStats constructor

## NOT In This Bundle (Do Not Fix)

The `convert::pipeline::*` failures (~35 tests) are **excluded** because those source files are too large to bundle (918KB for `stages.rs` alone). They will be addressed in a separate pass.

**Ignore all `convert::pipeline::*` entries in the failure list above.** Focus only on the `tui::*`, `disc::*`, and `db` failures.

## Your Task

Triage and fix the ~90 TUI/disc/db test failures. The approach should be:

1. **Group failures by root cause.** Many of these likely share common causes (e.g., a struct field added without updating test constructors, a function signature change, an enum variant renamed). Find the root causes rather than fixing tests one by one.

2. **Check for compilation errors first** — some test modules may fail to compile entirely, which would cause all tests in that module to appear as failures. Fix compilation errors first, then re-run to see the actual runtime failures.

3. **Distinguish test bugs from code bugs.** If a test fails because the test's expectations are wrong (e.g., it checks for old behavior that was intentionally changed), update the test. If a test fails because the code is actually broken, fix the code.

4. **Don't delete tests.** Adapt them to the current code. If a test is genuinely obsolete (tests removed functionality), it can be removed, but document why.

5. **Run `cargo test` after each batch of fixes** to verify progress and catch regressions.

The source files in this bundle are the current state of the codebase. Fix everything in place.
