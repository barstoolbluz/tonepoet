# Test Triage Pass 2: 76 Remaining TUI/Disc Failures

## Root Cause Groups (from panic output)

### Group A: XDG_CONFIG_HOME Mutex Poison (32 tests)

**Panic:** `XDG_CONFIG_HOME test lock: PoisonError { .. }` at `src/tui/test_support.rs:27`

**Tests:** All of `tui::theme::theme_builder_file_tests::*` (10), most of `tui::theme_builder::tests::*` (9), all of `tui::keybindings::progress_dialog_theme_tests::*` (9), plus 4 `tui::draw::theme_render_tests::*` (appearance_renderer, appearance_theme_name, draw_ui, wizard_fallback).

**Root cause:** One test in these suites panics first, poisoning the shared `XDG_CONFIG_HOME` mutex. All subsequent tests that try to acquire the same lock see `PoisonError` and fail. Fix the first real panic in each module and 32 tests likely pass.

**The first real panic is likely** `tui::draw::theme_render_tests::appearance_pane_renders_mockup_content_without_old_clutter` (panics with "old inline h/l instructions must not render" at `draw.rs:601`) — this one doesn't show PoisonError, so it's the one that poisons the mutex for the others.

### Group B: Disc Probe/Mapper (4 tests)

**Panic:** `left: InvalidPesPrefix, right: LpcmSubheaderIncomplete`

**Tests:**
- `disc::bluray_backend_libbluray::tests::lpcm_probe_reports_incomplete_subheader_for_started_pid`
- `disc::bluray_utils::tests::reports_parser_reason_for_incomplete_lpcm_subheader`
- `disc::bluray_backend_libbluray::tests::title_info_completion_frees_successful_pointer_when_event_follows` (panics with `assertion failed: err.contains("ENCRYPTED")`)
- `disc::bluray_mapper::tests::ffprobe_command_uses_injected_path_and_playlist_wide_audio_entries` (uncaptured panic — likely related disc probe/mapper change)

**Root cause:** The LPCM probe logic was changed to return `InvalidPesPrefix` where it previously returned `LpcmSubheaderIncomplete`. Either the code change is correct (update tests) or the code change is wrong (revert it).

### Group C: Disc Stream Summary Format Change (2 tests)

**Panic:** `left: "LPCM 24-bit 192kHz stereo", right: "LPCM 24-bit/192kHz stereo"`

**Tests:**
- `tui::disc_browser::disc_stream_summary_tests::stream_summary_sorts_by_codec_channels_depth_rate_and_caps_limit`
- `tui::draw_browse::folder_classification_info_pane_tests::classified_disc_folder_renders_existing_disc_summary_streams_and_cap`

**Root cause:** A format string changed from `24-bit/192kHz` (with slash) to `24-bit 192kHz` (with space). Either update the tests to match the new format or fix the format function.

### Group D: Browse Archive Search/Sort (6 tests)

**Panic:** Various — empty result sets where matches were expected, wrong sort orders.

**Tests:** `tui::browse::tests::archive_*` and `active_archive_*` tests (6 browse tests — the `performance_config_keys_update_archive_listing_settings` keybinding test also matches "archive" by name but is a PoisonError test in Group A).

**Sample panics:**
- `left: [], right: ["track.flac"]` — search returned no matches
- `left: ["a.flac", "b.flac"], right: ["b.flac", "a.flac"]` — sort order wrong
- `assertion failed: tags.tag_string.contains("correct artist")` — tag cache lookup failed

**Root cause:** The archive metadata search/sort path changed — probably `probe_cache` metadata lookup, `ArchiveTagCache`, or the search filter logic in `apply_browse_search`.

### Group E: Browse Probe/Navigation (3 tests)

**Tests:**
- `browse_preemphasis_checks_are_worker_side_only` — "fresh browse probes must not drop read_metadata errors and then repeat PE checks unconditionally"
- `probe_current_stops_when_selected_file_is_unstatable` — `assertion failed: !state.probe_pending.contains(&path)`
- `disc_directory_navigation_tests::tag_only_search_keeps_disc_directories_navigable_by_filename` — "directories, including disc directories, must remain filename-searchable for navigation"

**Root cause:** Browse probe logic or search filtering changed. Each may be independent.

### Group F: Browse DirStats Tokio Runtime (1 test)

**Panic:** `there is no reactor running, must be called from the context of a Tokio 1.x runtime`

**Test:** `tui::browse::browse_perf_followup_v10_tests::stale_active_recursive_dir_stats_are_cancelled_before_queueing_current`

**Root cause:** Test needs `#[tokio::test]` instead of `#[test]`, or the function it calls was changed to be async.

### Group G: Keybindings Inline Edit (16 tests)

**Tests:** All `tui::keybindings::inline_edit_behavior_tests::*`

**Sample panics:**
- Disc directory descent: `left: "/tmp/.tmpYNaRVB", right: "/tmp/.tmpYNaRVB/disc-source"` — browse didn't descend into the disc folder
- Gutter click: `left: [], right: ["/tmp/a.flac"]` — selection toggle didn't work
- Metadata inline edit: `left: None, right: Some(Artist)` — inline edit state not set
- Output inline edit: `left: Some(FolderTemplate), right: None` — inline edit state wrong
- Drag selection: `assertion failed: app.browse.drag_state.active` — drag state not activated
- Bulk guard: `expected bulk edit-tags confirmation, got Confirmation { ... ArchiveStartupRecovery ... }` — archive recovery overlay preempted the bulk guard

**Root cause:** Multiple issues — disc directory descent logic, inline edit state management, drag state, and test isolation (archive recovery detection interfering with other tests). The inline edit tests may share a common root cause in how `ConvertState` or `OutputOptionsState` inline edit fields are initialized or accessed.

### Group H: Command Tests (4 tests)

**Tests:**
- `bluray_toml_parser_reads_identity_tags_and_extension_fields` — extra field `presentation_extension` appears in `extra` map unexpectedly
- `bluray_custom_field_deletions_remove_stale_toml_keys` — "album-level field TAKE_NOTE has mixed values" error
- `execute_queue_does_not_publish_cue_metadata_before_successful_source_mode_update` — "probe failure branch should still return before Convert source publication"
- `bulk_guard_threshold_opens_confirmation_only_above_threshold` — `assertion failed: matches!(app.active_overlay, ActiveOverlay::None)`

### Group I: Draw Theme Render (1 real failure)

**Real failure:** `appearance_pane_renders_mockup_content_without_old_clutter` — "old inline h/l instructions must not render" at `draw.rs:601`

**Root cause:** The appearance pane rendering changed (theme builder redesign moved mode toggle to header, removed inline keybinding hints), but the test still checks for the absence of old instructions that may now be present in a different form. The other 4 `theme_render_tests` that fail with PoisonError are already counted in Group A — this test is the one that poisons the mutex for them.

### Group J: MusicBrainz Editor (3 tests)

**Panics:**
- `left: ["Whole Album", "", ""], right: ["Whole Album"]` — extra empty strings in album dimension
- `assertion failed: pos("TITLE") < pos("ARTIST")` — entry sort order wrong
- `assertion failed: ... entries.iter().find(|e| e.display_key == "TITLE").is_none()` — TITLE entry present when it shouldn't be

**Root cause:** The MusicBrainz editor entry population or sort logic changed.

### Group K: Keybinding Misc (1 test)

**Test:** `artwork_file_picker_handoff_tests::source_tree_has_no_app_local_file_picker_and_uses_crate` — "old in-app picker module must stay deleted"

**Root cause:** A file or module reference that the test checks for absence of may have been re-added, or the test's path check is wrong.

### Group L: Theme Builder Specific (3 tests — not mutex-poisoned)

**Tests:**
- `derived_keyboard_scroll_uses_rendered_visible_rows` — `left: 5, right: 0` — derived scroll not resetting
- `preview_lines_expand_when_card_height_has_room` — `assertion failed: ... contains("derived")` — preview content changed
- `inline_swatch_naming_records_actual_name_hitbox_only_while_active` — `left: " ", right: "["` — hitbox character mismatch

## Priority Order

1. **Group A first** — fix the 1-2 real failures that poison the mutex. This will unblock ~32 tests.
2. **Group G** (16 tests) — inline edit is the largest non-poisoned group
3. **Group D** (6 tests) — archive search/sort
4. **Groups B, C** (6 tests) — disc probe/format, likely simple assertion updates
5. **Everything else** — smaller groups

## Files to Modify

The source files in this bundle are the current state after the first triage pass. All files that contain failing tests are included. Fix everything in place.
