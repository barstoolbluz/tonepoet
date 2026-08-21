# tests — sanity assessment

444 of 444 read · 114 surprising

Each entry below is one **reading**, of a function or of a whole file. An
agent was given its name, signature, neighboring names and comments — never
its body — and wrote down what it expected to find. Then it opened the file.
The gap between the two is the finding. A file's own entry is titled `the file
itself` and asks whether the header at the top describes what is actually in
there.

`read at` is a hash of the body as it was when the reading was made. When it
stops matching the code, the reading is marked STALE and goes back in the
queue.

What this is and how to add to it: [README.md](README.md)

## tests/bluray_backend_smoke.rs

### the file itself
- spec 3 · read at `12ade90671fd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:15:38Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: An ignored-by-default smoke test file, run manually with BLURAY_ISO/BLURAY_DIR env vars pointing at real disc data, that opens the disc via libbluray from both an ISO and a BD-MV directory, runs shared assertions (smoke_one) to check title enumeration and that a title source's cursor survives metadata queries, and separately validates that real libbluray event errors from a fixture are surfaced/mapped correctly by the backend.
- found: Ignored smoke tests gated by env vars: one opens ISO and BD-MV dir paths via libbluray, enumerates titles/chapters/streams, checks LPCM bit-depth reporting per stream kind, and asserts a title source's read cursor is unaffected by concurrent metadata queries and that PTS continuity capability is reported unsupported; the other reads a fixture until a real libbluray fatal/read/encrypted event surfaces and asserts the resulting error string matches an expected category.
- predicted: most · documented: some · derivable: no · legible: not judged · trap: no

### `libbluray_opens_iso_and_bdmv_directory`
- spec 3 · read at `5a90bb542e6c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:11:55Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Reads the BLURAY_ISO and BLURAY_DIR env vars, and calls the smoke_one helper on each path to verify the libbluray backend can successfully open both an ISO file and a raw BDMV directory structure, likely panicking with a helpful message if the env vars aren't set.
- found: Reads BLURAY_ISO and BLURAY_DIR env vars (panicking via expect() with a helpful message if unset), then calls smoke_one on each path.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `smoke_one`
- spec 3 · read at `8f2c8067a786` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:43:19Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Shared helper invoked by both the ISO and BDMV-directory variants of the smoke test: opens the libbluray backend against `path`, queries the title list, asserts it's non-empty, and iterates the titles printing/asserting basic metadata (duration, chapter/clip counts) to sanity-check that the backend can read a real disc without panicking. Likely prints results since the test is run with --nocapture.
- found: Opens the libbluray backend at path, asserts titles are non-empty, calls a cursor-survival check, then for up to 8 titles prints metadata, verifies chapter count matches enumerated chapters, enumerates audio streams printing details, and asserts primary vs secondary LPCM streams report the expected structured bit-depth probing status (Probed/ProbeFailed/NotProbed for primary, NotProbed for secondary).
- predicted: most · documented: none · derivable: no · legible: most · trap: no

### `assert_title_source_cursor_survives_metadata_queries`
- spec 3 · read at `dc9de0804910` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:40:36Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Opens a read cursor/source for a title on the disc, reads some initial data from it, then issues one or more unrelated metadata queries against the disc (e.g. re-fetching title/chapter info), then reads more data from the same title source and asserts the second read continues from where the first left off (no reset, skip, or duplication) — proving that querying metadata does not disturb an in-progress title read cursor.
- found: Opens a title source, reads a warmup TS packet, records the cursor position and reads a comparison packet, rewinds to the cursor, then calls two different metadata-style queries (streams() on a different title, and pts_continuity_segments() capability check on this source) between the rewind and re-read, and asserts the re-read packet matches the earlier comparison packet — confirming the cursor position and stream content survive unrelated metadata calls.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `libbluray_surfaces_real_event_errors_from_fixture` — QUIRKY
- spec 3 · read at `9404c2e5989a` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:14:29Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Reads BLURAY_ISO/BLURAY_DIR env vars for a real disc fixture, determines the expected error event via first_event_error_from_fixture, opens the real libbluray backend against the fixture, and calls assert_real_event_error_matches to confirm the backend surfaces the same error. Likely a no-op/early return if the env vars aren't set, since it's an ignored smoke test.
- found: Reads BLURAY_EVENT_FIXTURE (required, path to an encrypted/damaged fixture), BLURAY_EVENT_EXPECT (optional, defaults to "any"), and BLURAY_EVENT_READ_LIMIT (optional byte cap, defaults 64MB) env vars, calls first_event_error_from_fixture to read the fixture until a fatal/read/encrypted event fires (panicking if none does within the limit), then asserts the resulting error matches the expected string via assert_real_event_error_matches.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `first_event_error_from_fixture` — QUIRKY
- spec 3 · read at `18ce4fec7b43` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:07:58Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Opens the Blu-ray disc/ISO at `path` via the backend, then loops reading events/packets up to `read_limit` times, returning Some(error_message) as soon as an event/read call surfaces an error, or None if it completes read_limit iterations without hitting one. Used by the smoke test to verify real event errors are surfaced correctly.
- found: Opens the disc and lists titles, then for up to the first 8 titles opens each as a source, seeks to start, and reads bytes (in up-to-1MB chunks) until read_limit total bytes or EOF, returning the first error encountered as Some(String) at any step, or None if everything succeeds.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `assert_real_event_error_matches` — QUIRKY
- spec 3 · read at `44e8856258f8` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:18:59Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A test helper that compares an error message produced by a real libbluray event against an expected string, likely using a substring/contains check (since exact wording from the C library may vary) and panics with a descriptive assertion message including both strings if they don't match.
- found: First asserts the error string looks like a genuine libbluray-event-derived error (contains LIBBLURAY/READ_ERROR/ENCRYPTED markers). Then `expected` is a category keyword (any/encrypted/read_error/fatal, presumably from a BLURAY_EVENT_EXPECT env var) selecting which further substring pattern must appear, panicking on an unrecognized category.
- predicted: some · documented: none · derivable: no · legible: full · trap: no

## tests/chunk2_orchestrator_contract.rs

### the file itself
- spec 3 · read at `95400eaa8a6f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:09:15Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A source-level "architecture contract" test suite that doesn't run the orchestrator end-to-end but instead reads the actual .rs source files as text and asserts structural/textual invariants about them, so it can run without audio fixtures. Helpers like source_without_cfg_test_items (strip out #[cfg(test)] modules), brace_delta (track nesting depth), and source_between (extract the substring of a function/block between two markers by balanced braces) implement a mini text-scanning toolkit used by the individual test functions. Each named test then greps/parses a specific source file to assert an invariant like "single files and queue items share the same scheduler work graph," "every external process spawn goes through ToolRunner (not raw Command)," "the compatibility/legacy orchestrator's metadata gate matches the new scheduler's gate," and "queue items carry full pipeline settings rather than a legacy/partial projection" — guarding against regressions where someone reintroduces a second code path, bypasses ToolRunner, or fakes/duplicates metadata ownership during the Chunk 2 orchestrator-unification refactor.
- found: Matches prediction closely: source_without_cfg_test_items/brace_delta/source_between are exactly the text-scanning toolkit I expected (strip test modules via brace counting, slice source between two markers). Each test include_str!s specific production files and asserts substring/token presence to lock in invariants — same scheduler work graph for single files vs queue, no direct process::Command spawning outside ToolRunner, metadata gate parity between scheduler and compatibility orchestrator paths, full legacy option-surface coverage in the settings builder, and PipelineRequest literals always setting worker_count. I underestimated how literal/brittle some assertions are (checking exact multi-line source snippets like specific field-assignment lines) rather than higher-level structural checks.
- predicted: most · documented: full · derivable: no · legible: not judged · trap: no
- note: None.

### `source_without_cfg_test_items`
- spec 3 · read at `3a500fea5d29` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:11:11Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Scans the source text for #[cfg(test)] attributes, and for each one uses brace_delta/source_between to find and remove the following brace-delimited item (mod/fn block) from the string, returning the source with all test-only code stripped so downstream architectural-contract regex/text checks don't false-positive on test code.
- found: Line-by-line scan: keeps lines normally, but when it sees a bare "#[cfg(test)]" line followed by a "mod " line, it skips the mod line and all subsequent lines until brace_delta tracking shows the block has closed (depth back to 0), effectively dropping only #[cfg(test)] mod blocks, not arbitrary test-attributed items.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: I overpredicted generality (assumed it strips any #[cfg(test)]-attributed item like fns) and wrongly assumed source_between was used here — it's actually unused by this function, only brace_delta is.

### `brace_delta`
- spec 3 · read at `86544e74a986` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:12:09Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Counts the net brace balance in a line of source text by counting '{' characters minus '}' characters, returning the delta as an i32; used by these contract tests to scan checked-in source and track nesting depth (e.g. to find the span of a function body) without a full parser.
- found: Folds over the line's characters, incrementing on '{' and decrementing on '}', returning the net delta.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `source_between`
- spec 3 · read at `7ad390bfcb2b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:36:22Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Finds the byte index of start_marker in contents, then finds end_marker after that point, and returns the substring slice between them (after the start marker, before the end marker) — panicking or expecting if either marker is missing. Used by these contract tests to extract a specific function/block's source text from a source file for pattern matching.
- found: Finds start_marker's byte offset, slices from there, then finds end_marker within that remainder and returns the slice from start_marker (inclusive) up to end_marker (exclusive), panicking with a message naming the missing marker if either is absent.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: I predicted the slice would begin after the start marker; it actually begins at the start marker itself (inclusive), which matters for callers checking the marker text is present in the extracted region.

### `single_files_enter_the_shared_pool_as_immediate_work_units`
- spec 3 · read at `01b6c5d26f17` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:43:17Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Uses source_between/brace_delta helpers to extract the relevant scheduler function's text from the checked-in source, then asserts (via string/pattern matching) that single-file inputs are enqueued directly into the shared work pool as immediately-ready units rather than routed through a separate legacy path — a static contract check on source text, not a runtime test.
- found: Reads processor.rs source via include_str!, asserts presence of several specific function/symbol names (WorkKind::SingleFile, build_single_file_work, etc.), then locates the SourceKind::SingleFile branch and checks its nearby text calls build_single_file_work and does NOT call prepare_pipeline_item_for_scheduler — confirms the static source-text contract-check approach I predicted, with the specific exclusion check being the detail I didn't call out.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `direct_process_item_uses_the_same_shared_scheduler_graph_as_queue_processing`
- spec 3 · read at `450a32ae9877` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:23:00Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A source-inspection test that extracts the body of the "direct process item" function from checked-in source (using helpers like source_between/brace_delta) and asserts it references the same shared scheduler-graph construction function that the queue-processing path uses, guarding against the two entry points drifting into separate implementations.
- found: Includes processor.rs source as a string, finds process_item's body, asserts it calls run_single_item_with_shared_scheduler and that the file also contains a run_queue_with_shared_orchestrator(vec![item] call, and asserts process_item does NOT call the legacy run_pipeline_item_with_tool_paths.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `normal_request_builder_requires_full_pipeline_settings_handoff`
- spec 3 · read at `64a204f66d3b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:04:20Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A source-text contract test (not a runtime test) that reads the orchestrator's "normal request" builder function's source text (likely via source_between/brace_delta helpers), and asserts the extracted body references the full PipelineSettings struct/handoff rather than a legacy partial projection, failing if someone reintroduces a stripped-down settings path.
- found: Uses include_str! to load unified_request.rs source, slices out the build_pipeline_request function body via string search, and asserts it mentions full PipelineSettings handoff while not referencing a legacy per-item settings function; also asserts other helper/function names exist in that file and processor.rs.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `scheduler_failure_accounting_waits_for_all_terminal_track_records`
- spec 3 · read at `c7d9da32d042` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:12:54Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A source-text contract test that extracts the scheduler's failure-accounting function body (via source_between/brace_delta helpers) and asserts textually that it only finalizes/reports failure counts after all track records reach a terminal state, rather than short-circuiting on the first failure.
- found: Asserts, via literal substring checks on the raw source of processor.rs and scheduler.rs, that specific accounting phrases exist (e.g. 'pending.finished >= pending.expected', 'cancel_requested', sorted outputs) and that a removed-on-cancel pattern is absent, plus that two specific named unit tests exist in scheduler.rs by name — a purely textual/architectural pin, not a behavioral check.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `scheduler_has_one_shared_work_graph_for_source_and_track_units`
- spec 3 · read at `82a7edc87d12` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:54:01Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A static "contract" test that reads the scheduler's source code as text (via helpers like source_between/brace_delta) and asserts there's only one shared work-graph/queue data structure used for both source-level and track-level units, rather than two separate structures — checking for the absence of a second graph field or duplicate scheduling logic via string/substring assertions on the source.
- found: Reads processor.rs, scheduler.rs, and stages.rs as raw strings and asserts processor.rs uses a single SharedWorkerPool (not stages.rs, and no separate JoinSet), then checks that both processor and scheduler mention every work-unit kind (SingleFile, ArchiveExtract, etc.), plus checks for AlbumReadiness::Failed and job_cancel.cancel() presence.
- predicted: most · documented: most · derivable: no · legible: full · trap: no
- note: Confirmed my guess that it's a text-based architectural contract test, though the specific checks (exact pool type name, work-kind enumeration, cancellation/failure-path presence) were more concrete than what I predicted.

### `planner_metadata_satisfaction_is_derived_from_planner_owned_effects`
- spec 3 · read at `d58f5feb2374` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:31:34Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A source-text contract test that reads the checked-in orchestrator source (via source_between/brace_delta helpers), extracts the function/method responsible for determining whether metadata requirements are satisfied, and asserts (via string matching) that it derives satisfaction from planner-owned effects/state rather than some other bypassing mechanism, guarding against future regressions that fake metadata ownership.
- found: Includes the raw source of plan_bridge.rs, track_executor.rs, and stages.rs at compile time and asserts specific identifier substrings are present (e.g. effective_metadata_satisfaction, planner_metadata_obligations_for_track) and one specific pattern is absent (settings.metadata.transfer_tags = false), guarding that metadata satisfaction flows through planner-owned effects rather than a legacy flag.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: I predicted use of the file's own source_between/brace_delta helpers for scoped extraction, but this particular test just does whole-file include_str! + substring assertions.

### `legacy_compat_pipeline_settings_cover_the_legacy_option_surface_explicitly`
- spec 3 · read at `7ea567d7fd68` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:17:59Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: This is a source-text contract test (not a runtime test): it reads the relevant source file(s) as a string and checks that the legacy-compat construction of PipelineSettings explicitly lists every legacy option field by name, rather than using `..Default::default()` or similar shorthand that could silently drop or default a field. It likely asserts the presence of each expected legacy field name string within the matched source region, failing if any is missing.
- found: Reads unified_request.rs as a string and asserts it contains a long explicit list of legacy setting field/token names (format, resample, per-codec options, sox/soxr/ssrc resampler params, metadata, verification, replaygain), guarding against a legacy option silently being dropped from the unified settings builder.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `every_external_process_boundary_runs_through_tool_runner_modules`
- spec 3 · read at `29203ea67658` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:59:20Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Scans the crate's source files as text (using helpers like source_without_cfg_test_items and source_between to strip test-only code and locate relevant regions), searching for direct external-process invocations such as std::process::Command::new, and asserts that any such usage only occurs within the designated tool_runner module(s), preventing other code from bypassing the ToolRunner abstraction for spawning external tools.
- found: For four specific orchestrator source files, strips test-only code via source_without_cfg_test_items and asserts the remaining production code contains no literal "std::process::Command" or "tokio::process::Command" strings, ensuring those files never spawn processes directly instead of going through ToolRunner.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `compatibility_orchestrator_metadata_gate_matches_scheduler_gate`
- spec 3 · read at `af71bdd54b78` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:33:58Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Uses source_between/brace_delta helpers to extract the metadata-gate condition or expression from both the compatibility orchestrator source and the scheduler source, then asserts they are textually identical (or that both delegate to the same shared helper function name), preventing the two gate implementations from silently drifting apart.
- found: Extracts the two named function bodies from stages.rs via source_between, then for each of them asserts the body contains the same four specific substrings (planner_metadata_already_satisfied call, artifacts.expect, source.expect, and &req usage), ensuring both the scheduler path and compatibility orchestrator path independently honor the same metadata-gating contract rather than comparing the two bodies against each other directly.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `queue_items_carry_full_pipeline_settings_without_legacy_projection`
- spec 3 · read at `12fc7ad825b2` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T02:21:36Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A source-text contract test — reads the relevant orchestrator/queue source file as a string (via source_between/source_without_cfg_test_items helpers) and asserts that queue-item construction uses the full PipelineSettings struct directly, while asserting the source does NOT contain any legacy "projection" conversion (e.g. a narrower legacy settings type or a .into_legacy()-style call), via simple contains/!contains string assertions rather than executing real code.
- found: Uses include_str! on four source files (formats.rs, queue.rs, unified_request.rs, processor.rs) and asserts each contains specific literal strings confirming pipeline_settings is a full Option<PipelineSettings> field threaded through specific constructor/setter names and assignment expressions — a pure text-presence check, no legacy-absence assertions.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `pipeline_request_literals_include_worker_count`
- spec 3 · read at `b94e726ab62c` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:58:07Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Uses source_between/brace_delta helpers to extract text of PipelineRequest struct-literal construction sites in the checked-in source, and asserts each one contains a worker_count: field, guarding against a regression where worker_count is dropped from request construction.
- found: Manually scans three specific source files for "PipelineRequest {" occurrences, skips ones that are struct/fn/impl definitions, and asserts each remaining literal site contains "worker_count:" within 1400 bytes.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

## tests/chunk_2_1_2_manifest_rerun.rs

### the file itself
- spec 3 · read at `d643a3d5bac3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:09:30Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Integration tests for the conversion pipeline's manifest-based "rerun" / incremental-publish feature: a manifest records prior conversion outputs (album-relative paths, source hashes) so that a subsequent run can detect unchanged sources and skip re-encoding (matching_manifest_skips_without_conversion) while a changed source forces redo. Tests cover manifest serialization round-tripping with album-relative paths, rejecting unsafe absolute/parent-escaping output paths, rejecting a manifest read against the wrong album dir, hashing-policy-aware refresh, atomic publish via temp-dir-then-rename (and stale temp cleanup matching the real prefix), and use a CountingVerifier test double to assert how many times/when verify_existing_output is invoked, including a case where the actual album dir (not the manifest's recorded one) must be used for verification.
- found: Integration tests for the manifest/rerun module: album-relative output path round-trip and validation (rejecting absolute/escaping paths), album-dir mismatch detection, hashing-policy-aware publish refresh, atomic temp-dir publish + rename survival, decide_rerun skip/redo behavior on unchanged vs changed source, stale publish temp-dir cleanup by prefix, and a CountingVerifier spy proving verification uses the actual album dir rather than the manifest's recorded one.
- predicted: full · documented: none · derivable: yes · legible: not judged · trap: no

### `settings`
- spec 3 · read at `a188ca3630f0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:51:52Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Returns a baseline PipelineSettings fixture (likely PipelineSettings::default() or a small struct literal with sensible test defaults) shared by the manifest-rerun tests in this file.
- found: Returns PipelineSettings::default(), exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `make_track`
- spec 3 · read at `149b1a4d31ef` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T00:36:09Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Test helper that writes output_bytes to a file at album_dir joined with relative_output, then constructs and returns a ConversionManifestTrack populated with that relative output path plus computed metadata (e.g. byte size and/or content hash) matching the written file, for use in manifest round-trip/rerun tests.
- found: Writes a fake source.wav and the given output_bytes at relative_output under album_dir, then builds a ConversionManifestTrack::new with source metadata, a fixed TrackIdentity, settings fingerprint, fixed converter/plan-hash strings, output size, and ValidationStatus::Passed, for use as manifest test fixtures.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `make_manifest`
- spec 3 · read at `f7aee8faeb74` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:32:35Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Small test helper that builds a minimal ConversionManifest for the given album_dir, likely delegating to make_track and default settings, to avoid repeating manifest construction boilerplate across this file's test cases.
- found: Constructs ConversionManifest::new using album_dir, settings(), and a single make_track entry ("01.flac") — exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `manifest_round_trip_keeps_album_relative_output_paths`
- spec 3 · read at `04e65c999f32` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:37:33Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Builds a manifest (via make_manifest/make_track helpers) with a track whose output path is relative to the album dir, writes/publishes it to a temp dir, then reads it back and asserts the output path is still stored/interpreted as relative to the album directory rather than being turned into an absolute path.
- found: Creates an album dir, builds a manifest via make_manifest, writes it with write_manifest (asserting the path is .tonepoet-manifest.json), reads it back with read_manifest, and asserts the first track's output_path is still the relative PathBuf "01.flac" rather than absolute.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `absolute_or_parent_output_paths_are_rejected`
- spec 3 · read at `07742bae044f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:46:32Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A test constructing a manifest whose track output path is either absolute or contains a parent-directory (..) component, then verifying that reading/parsing that manifest returns an error rather than accepting the path, since output paths must stay relative and confined within the album directory.
- found: Directly tests validate_album_relative_output_path: a "../escape.flac" path returns OutputPathEscapesAlbum error, and an absolute path returns OutputPathNotRelative error.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `read_manifest_rejects_mismatched_album_dir`
- spec 3 · read at `ad53989789ec` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:40:32Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Builds a manifest (via make_manifest) whose recorded album_dir field doesn't match the directory it's actually stored/read from, writes it to disk, then calls the manifest-reading function and asserts it returns an error (or None) rather than silently accepting the mismatched path.
- found: Creates a manifest for one album dir, writes its raw JSON into a different directory (simulating a copied/edited manifest file), then asserts read_manifest on that other dir returns a specific Err(ManifestError::AlbumDirMismatch) variant.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `refresh_manifest_for_publish_honors_hashing_policy`
- spec 3 · read at `e77caafe7aec` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:59:33Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Sets up a track/manifest pair and calls refresh_manifest_for_publish with a specific hashing policy (e.g. skip-hash vs verify-hash), then asserts the resulting manifest's hash fields reflect that policy — either populated with a recomputed hash or left absent/unchanged — showing the refresh respects the caller's chosen hashing behavior rather than always hashing.
- found: Writes a published file into a temp publish dir, calls refresh_manifest_output_facts_for_publish with hashing=false (asserts output_hash stays None but output_size is set) then with hashing=true (asserts output_hash now matches the real file's sha256), confirming the bool flag controls whether hashing is performed.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `manifest_survives_temp_dir_atomic_publish_rename`
- spec 3 · read at `6ca8d90f63d2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:52:07Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Simulates a publish flow where output files are first written to a temp directory then atomically renamed into the final album directory; the test verifies that a manifest written referencing final paths still matches on a subsequent rerun (skip-without-reconversion) even though the actual files went through the temp-dir-then-rename publish path, confirming the manifest isn't tied to transient temp paths.
- found: Writes an output file and refreshes manifest facts inside a temp album dir, writes the manifest for publish, then deletes the final dir and renames temp-to-final; asserts the reread manifest reports the final album_dir and a relative output_path (not the temp path), confirming manifest paths survive the atomic rename.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `matching_manifest_skips_without_conversion`
- spec 3 · read at `5a51c6273971` · commit `1681528` · read by claude-sonnet-4.5 · asked for sonnet · via claude · when 2026-08-19T07:02:21Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This test constructs a track and a manifest (via make_track/make_manifest helpers) that already matches the track's current state (same hash/settings), then runs the rerun/publish logic and asserts that no conversion occurred — likely by checking a call counter or that the output file wasn't touched/rewritten. It's verifying the "skip unchanged" optimization path of the manifest-based caching system.
- found: Writes a manifest to a temp album dir, then calls decide_rerun with SkipIfManifestMatch policy and asserts the result is RerunDecision::Skip; also checks an unused atomic counter that was never wired to anything, staying 0.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: conversion_count is declared but never actually connected to a conversion call — the assertion on it is vacuous, it's testing decide_rerun's return value only, not that no conversion function was invoked.

### `changed_source_forces_redo`
- spec 3 · read at `2f35160d5fd2` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:57:10Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: This test builds a manifest recording a source file's hash/mtime (via make_manifest/make_track), then modifies the actual source file's contents on disk, and asserts that the manifest-comparison logic now reports the track needs re-conversion rather than being skipped — the inverse of the matching_manifest_skips_without_conversion peer test.
- found: Writes a manifest for an album dir, then writes a source.wav with different content, then calls decide_rerun with OverwritePolicy::SkipIfManifestMatch and asserts the result matches RerunDecision::Redo (panicking with the actual value otherwise).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `stale_publish_temp_cleanup_matches_real_tmp_prefix`
- spec 3 · read at `5c1afaffa964` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T00:26:05Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Test asserts that the prefix/pattern used by stale-temp-file cleanup logic matches the actual naming convention of temp files created during atomic publish, so a real temp file produced by the publish path would actually be recognized and cleaned up rather than the cleanup pattern silently drifting out of sync.
- found: Test creates a real-format stale publish temp dir (.Album.tmp-123) and an unrelated dir with a similar but different suffix (.Album.partial-123), calls delete_stale_publish_temp_dirs, and asserts only the matching-prefix dir was deleted while the unrelated one survives.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Confirms these are directories, not files, and that the cleanup function is scoped/keyed by album_dir name to build the expected prefix.

### `verify_existing_output`
- spec 3 · read at `c756a4974454` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T01:57:11Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A test mock that increments an internal call counter (interior mutability, e.g. Cell<usize> or AtomicUsize) each time it's called, ignoring the unused path/settings params, and always returns Ok(()) to simulate successful verification so tests can assert how many times verification was invoked during a manifest rerun.
- found: Increments an atomic call counter (fetch_add with SeqCst ordering) and always returns Ok(()), ignoring the path/settings arguments entirely — exactly a call-counting test stub.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `verify_uses_actual_album_dir_not_manifest_album_dir` — QUIRKY
- spec 3 · read at `47d6ed840151` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:18:18Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Creates a manifest whose stored album_dir path differs from the actual current directory the album lives in (e.g. after a move/rename), then runs the manifest-rerun verification and asserts the code checks/verifies outputs using the real, current album directory path rather than the stale one recorded inside the manifest.
- found: Creates a fresh temp album_dir, builds a manifest via make_manifest(&album_dir), then calls verify_manifest_outputs_at_album_dir passing that same album_dir explicitly (rather than relying on any path field inside the manifest), and asserts the CountingVerifier was invoked exactly once — confirming the function's album_dir parameter, not a manifest-internal path, drives where verification looks.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

## tests/chunk_2_1_2_orchestrator_gate.rs

### the file itself
- spec 3 · read at `8000b8381feb` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:15:05Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Integration test file for the "orchestrator gate" logic in a publish/conversion pipeline — deciding whether to skip, verify, or replace-backup an existing output before doing conversion work. Sets up fake settings/publish policy and a FakeVerifier test double, builds a fixture album+manifest via write_album_with_manifest, and asserts three behaviors: skip policy short-circuits without running verification/conversion, verify policy only short-circuits when the verifier reports success, and a verify failure falls through to a replace-with-backup path instead of failing outright.
- found: Integration test file for evaluate_album_rerun_gate: tests that SkipIfManifestMatch skips without calling the verifier, VerifyIfManifestMatch calls the verifier and skips only on success, and a verifier failure downgrades the decision to Continue with an effective policy of ReplaceWithBackup plus a warning. Includes a FakeVerifier test double and fixture helpers for building an album dir with a matching manifest.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `verify_existing_output`
- spec 3 · read at `1082f7412ee3` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:21:24Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Test-double implementation on FakeVerifier that records the call (e.g. pushes path into a Vec/records call count for assertions) and returns Ok(()) or a configured Err(ExistingOutputVerificationError) based on a field set up by the test (like a `should_succeed` bool or per-path outcome map), letting tests exercise both the verify-success short-circuit path and the verify-failure-continues-with-backup path.
- found: Increments an atomic call counter, then returns Err(DecodeFailed) with a fake reason if self.fail is set, else Ok(()).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `settings`
- spec 3 · read at `a188ca3630f0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:51:54Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A test helper returning a default/minimal PipelineSettings instance, likely via PipelineSettings::default() or a struct literal with baseline field values, reused across the file's skip/verify policy tests.
- found: Returns PipelineSettings::default(), a trivial test fixture helper.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `publish_policy`
- spec 3 · read at `1c37f77b72ec` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:32:37Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Test helper: constructs and returns a PublishPolicy with the given overwrite policy set and other fields at their default values, to reduce boilerplate in the orchestrator-gate tests.
- found: Constructs PublishPolicy with the given overwrite value and both other fields (same_filesystem_required, write_manifest) hardcoded to false — matches prediction exactly.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `write_album_with_manifest`
- spec 3 · read at `33d6250bb9e5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:12:58Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A test-fixture helper that creates the given album directory (if needed), writes one or more placeholder source audio files into it, and writes an accompanying manifest file (JSON/TOML) describing those files/tracks — used to set up fixtures for the orchestrator gate policy tests (skip/verify short-circuit tests).
- found: Creates the album dir, writes a placeholder source.wav and output 01.flac, builds a ConversionManifestTrack with specific identity/fingerprint/status fields, wraps it in a ConversionManifest, and writes it out via write_manifest — a fixture for orchestrator gate tests.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `skip_policy_short_circuits_before_conversion`
- spec 3 · read at `09d90758bf84` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:18:44Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: This test configures the orchestrator with a "skip" publish policy (skip existing output) and an existing output file, then runs the pipeline and asserts the actual conversion step is never invoked — the gate short-circuits before doing any conversion work, likely checked via a fake converter's call count being zero.
- found: Writes an album with a manifest, then calls evaluate_album_rerun_gate directly with a SkipIfManifestMatch policy; asserts the gate decision is Skip{verified:false} and that neither the verifier nor conversion counter were ever invoked — confirming the gate decides to skip based on manifest match alone, without calling the verifier.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `verify_policy_short_circuits_only_after_verifier_success`
- spec 3 · read at `3f8bd9bcf499` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:58:37Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Test: sets up an album with an existing output/manifest and a FakeVerifier, configures publish_policy to use verify-then-skip semantics, and runs the orchestrator. It asserts that when the FakeVerifier reports the existing output is valid, the orchestrator short-circuits (skips re-conversion) — but that this short-circuit only happens after/because of a successful verify_existing_output call, distinguishing it from unconditional pre-conversion skip logic tested elsewhere.
- found: Calls evaluate_album_rerun_gate directly with a VerifyIfManifestMatch policy and a non-failing FakeVerifier against an album with an existing manifest, asserting the decision is Skip{verified:true} and that the verifier was invoked exactly once.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `verify_failure_continues_with_replace_backup`
- spec 3 · read at `e9f5e467596e` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:45:56Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This async test sets up a FakeVerifier configured to report a verification failure, builds settings/publish_policy using write_album_with_manifest to create a fake album, then runs the orchestrator gate logic. It asserts that even though verification failed, the orchestrator still proceeds to publish using a "replace with backup" strategy (i.e. verify failure doesn't hard-abort when that publish policy is in effect) — probably checking that the original file was backed up and replaced, and some failure/warning status is recorded.
- found: Sets up an album, a FakeVerifier configured to fail, and calls evaluate_album_rerun_gate with an initial policy of VerifyIfManifestMatch. Asserts the gate decision is Continue with a warning, and that the effective overwrite policy was downgraded to ReplaceWithBackup since verification failed.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The core mechanic — that a failed verify downgrades VerifyIfManifestMatch to ReplaceWithBackup rather than aborting — isn't guessable from the name alone, but reads clearly once seen.

## tests/chunk_2_1_2_transactional_state.rs

### the file itself
- spec 3 · read at `bb3159868682` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:09:21Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Test file verifying transactional/atomic state-file handling for some tonepoet operation (likely archive conversion or extraction): that in-progress/temp files use distinctive tonepoet-owned suffixes so they're identifiable, that an incomplete/partially-validated final state is hidden from consumers until it's fully validated (not left as a misleading partial output), that cleanup routines only ever delete files matching those known tonepoet-owned suffixes (never arbitrary user files), and that detection logic for "is this a tonepoet state file" only matches those specific suffixes rather than being overly broad.
- found: Matches prediction closely: tests the partial→validated→final state machine for track output during (presumably) audio conversion, confirms suffix-based ownership (PARTIAL_SUFFIX/VALIDATED_SUFFIX) so tonepoet's own transactional temp files are distinguishable from user files, and confirms cleanup/detection only touch tonepoet-owned suffixes. I correctly guessed the "convert" context generically but didn't know it was specifically the convert::pipeline module.
- predicted: full · documented: none · derivable: yes · legible: not judged · trap: no

### `transactional_state_paths_use_tonepoet_owned_suffixes`
- spec 3 · read at `221edd69ce72` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:59:47Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A test that generates the transactional state/backup/temp file path(s) for a given target file and asserts the resulting path ends with a distinctive tonepoet-owned suffix (e.g. ".tonepoet.tmp" or similar), ensuring cleanup logic can safely identify and only delete files tonepoet itself created.
- found: Calls transactional_track_paths on a sample path and asserts partial_path/validated_path carry PARTIAL_SUFFIX/VALIDATED_SUFFIX appended to the original filename, while final_staged_path equals the original unsuffixed path.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `partial_validated_final_state_machine_hides_incomplete_output` — QUIRKY
- spec 3 · read at `1a4563af673b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:05:49Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Sets up a scenario where an output file write is only partially complete (e.g. a temp/staging file with a tonepoet-owned suffix exists, but the atomic rename to the final filename hasn't happened or validation hasn't passed), then asserts that the final output path is not present/visible — the state machine correctly hides the incomplete/unvalidated output rather than exposing a partial file as if it were done.
- found: Walks a three-stage transactional output state machine (begin_track_output → mark_track_validated → materialize_validated_final), asserting at each step that only the current stage's path exists and prior-stage paths are gone, ending with the final file containing the written bytes.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `cleanup_deletes_only_known_tonepoet_state_paths`
- spec 3 · read at `aff2fcdcf077` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:56:48Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: This test creates a temp directory containing files with tonepoet's known state-file suffixes plus unrelated/foreign files, invokes the cleanup routine, then asserts that only the recognized tonepoet state paths were deleted while other files survive untouched.
- found: Creates a tonepoet-owned .partial and .validated file next to a final track path plus lookalike user files with similar suffixes but different base names, runs delete_stale_transactional_track_states, and asserts only the two tonepoet-owned files (matching final_path's derived paths) are deleted while the user's own .partial/.validated files survive.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `state_file_detection_matches_only_tonepoet_owned_suffixes`
- spec 3 · read at `d2be8379900d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:32:58Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Unit test asserting a state-file detection function returns true only for filenames carrying tonepoet's own transactional suffixes (e.g. .tonepoet-stage-NN, .tonepoet-final) and false for similar-looking but unrelated filenames, to guard cleanup logic from touching non-tonepoet files.
- found: Asserts is_transactional_state_file returns true for filenames with PARTIAL_SUFFIX or VALIDATED_SUFFIX appended (tonepoet's own transactional state markers) and false for unrelated filenames that merely resemble them ('notes.partial', 'take.validated').
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

## tests/conversion_action_runscript_containment.rs

### the file itself
- spec 3 · read at `f76df5b5bd4b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:15:55Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A dense integration test suite for a process-containment/supervisor system that runs external "runscript" hooks during conversion actions, using a Fixture helper to spawn and observe processes. Tests cover exec semantics (literal argv/env/stdin), cgroup containment without control-fd leaks, timeout handling involving setsid/double-fork and term-ignoring grandchildren, crash/cancellation recovery, bounded output capture truncation, and supervisor failure edge cases like PID reuse or remote-host recovery not signaling local processes — verifying the containment boundary holds even under adversarial process behavior.
- found: Linux/macOS-only integration suite for the script_supervisor containment system: a Fixture builds a shell script and private runtime dir, and tests cover literal argv/env passing with null stdin, working-directory retention across path replacement, exit-code and stderr-tail preservation, cancellation forcing termination of the whole process domain, detection/draining of backgrounded pipe-inheriting children, bounded 64KB output capture truncation, fail-closed setup on runtime-identity mismatch, ACK-gated durable-journal prepare rejection blocking exec, remote-host recovery never signaling local processes, forced supervisor fallback and required-cgroup arming (Linux-only), setsid double-fork timeout escalation, and — via an actual spawned+SIGKILLed driver subprocess — recovery of live containment after the supervising application itself crashes, plus PID-reuse/start-identity-tick detection forcing manual recovery.
- predicted: most · documented: none · derivable: yes · legible: not judged · trap: no

### `new`
- spec 3 · read at `18763be5d55f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:40:23Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Creates a temporary directory and writes `body` to a script file inside it (prefixed with a shebang like #!/bin/sh if not already present), sets the file executable via fs permissions, and returns a Fixture struct holding the tempdir (to keep it alive) and the script path for later use by test helpers like `command`.
- found: Writes an executable shell script with a shebang+set -eu wrapper into a tempdir, chmods it 0700, and also creates and chmods a separate private "runtime" subdirectory, recording its device/inode as a RuntimeDirectoryIdentity alongside the script path and tempdir handle.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `command`
- spec 3 · read at `512384d40fcd` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:30:24Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A Fixture test-helper builder method that constructs a SupervisedCommand value using args and timeout together with fixture state (likely paths/config set up in Fixture::new, such as working directory, script path, or cgroup config) — a convenience constructor used across the containment tests instead of repeating boilerplate SupervisedCommand construction in every test.
- found: Builds a SupervisedCommand for the fixture's retained script, opening retained file descriptors for the script and working directory (for containment-safety against path replacement), fixed sanitized environment (PATH + a test marker var), Auto containment preference, and pointing helper_executable at the built tonepoet test binary.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `next_token`
- spec 3 · read at `5e2d2c9238bd` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:22:49Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A helper that generates a unique token string, likely backed by a static atomic counter incremented each call and formatted as a string, used to create unique identifiers/paths/markers across test cases in this containment test suite.
- found: Combines the current process id (shifted into the high 64 bits) with an atomically-incremented counter (low bits) into a single u128, formatted as a 32-char hex string — giving uniqueness both across processes and within a process, not just a plain counter.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `run_collect`
- spec 3 · read at `bd5677a80c08` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:34:40Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Test helper that sets up an event-collecting channel/sink, invokes the script supervisor's run entry point on command with cancelled as the cancellation flag, drains emitted ScriptLifecycleEvents into a Vec, and returns both the SupervisedOutcome and the collected events, propagating ScriptSupervisorError on failure.
- found: Calls run_supervised with a closure checking cancelled via Acquire load and a closure that clones/pushes each event into a Vec, then returns the outcome paired with collected events.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Confirms the callback-closure style API of run_supervised (cancellation predicate closure + per-event callback closure) rather than a channel/sink.

### `assert_containment_terminal` — QUIRKY
- spec 3 · read at `c4ab356fd565` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:58:03Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A test helper that scans the events slice for a terminal containment/cleanup event (e.g. process group fully reaped or supervisor exited), asserting it is present and likely that it's the last event or that no further activity follows — used by multiple containment tests to confirm the run-script sandbox correctly terminated and released all child processes.
- found: Asserts three specific ScriptLifecycleEvent variants are all present somewhere in the events slice: ContainmentPrepared, ContainmentEmpty, and OutputCaptureCompleted — not a single terminal-state check but three separate existence assertions with exact variant names I couldn't have guessed.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `direct_exec_preserves_literal_argv_environment_and_null_stdin`
- spec 3 · read at `01d5a6c5be3c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:07:31Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Spawns a process directly (bypassing any shell wrapper) via the Fixture/run_collect helpers, passing argv elements containing shell metacharacters and custom environment variables, then asserts the child observed the exact literal argv (no shell interpretation), saw the expected environment variables, and had stdin connected to /dev/null rather than inherited from the parent.
- found: Runs a supervised shell fixture script with two argv args containing spaces and shell metacharacters plus an env var, verifying the script sees them literally unshelled (no injection), stdin is closed (read fails), and asserts the full supervision outcome: success, no timeout/cancel, script released, empty containment, no background descendants, and correct stdout.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `retained_working_directory_survives_path_replacement`
- spec 3 · read at `04fb4c23feb7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:08:38Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Opens/retains a working-directory handle (fd) before spawning the contained child process, then replaces or renames the directory path on disk, and asserts the running child still operates against the originally-retained directory rather than the new path—proving containment holds a retained fd immune to path-swap TOCTOU issues.
- found: Opens a File descriptor on the working directory before renaming it to a new "retained" path and recreating an empty replacement at the original path; runs the command and verifies (via output files and a pwd marker) that the child actually ran in the renamed/retained directory (by fd), not the replacement directory at the original pathname.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `nonzero_exit_is_preserved_after_empty_proof`
- spec 3 · read at `98967f656207` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:43:27Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Sets up a Fixture running a script that exits with a nonzero code but produces no output/proof artifact (an "empty proof"), then calls run_collect and asserts the reported exit status is still the nonzero code — verifying the containment logic doesn't overwrite or mask a genuine failure exit code just because there was nothing to capture/prove.
- found: Runs a script that prints to stderr and exits 37, then asserts via run_collect that the outcome's exit code is 37, containment_empty is true, stderr_tail captured the printed text, and the event stream reaches a terminal containment state.
- predicted: most · documented: none · derivable: no · legible: full · trap: no
- note: The name's 'proof' maps to the outcome.containment_empty field, which isn't guessable from the test name alone without seeing the struct.

### `cancellation_terminates_the_complete_observed_domain` — QUIRKY
- spec 3 · read at `61ab72cbf7a1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:58:24Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Uses the Fixture harness to run a script that spawns nested/grandchild processes, signals markers via wait_for_path, then cancels the running action and asserts (via assert_containment_terminal) that the entire observed process domain — leader and all descendants — is actually terminated, not just the immediate child.
- found: Runs a script that ignores SIGTERM (trap '' TERM) and loops forever, using a marker file to know when the trap is armed before triggering cancellation (avoiding a race with cgroup-fallback setup). Asserts cancellation occurred, containment ended empty, and both a TerminationRequested and a ForcedTerminationRequested (escalation to SIGKILL) event fired, verified terminal via assert_containment_terminal.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `leader_zero_with_inherited_pipe_child_is_rejected_and_drained` — OBSCURE
- spec 3 · read at `e836aa6c31c5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:38:55Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A test where a supervised child reports a leader pid of 0 (an invalid/sentinel value) while it also has a grandchild that inherited a pipe fd from the parent. It asserts that the runscript containment logic rejects this as an invalid leader (fails the operation rather than trusting pid 0) while still draining/closing the inherited pipe so the test doesn't hang waiting on the pipe to close.
- found: Runs a script that spawns a backgrounded subshell (which traps TERM and sleeps) and then exits 0 itself, leaving a background descendant holding the inherited stdout/stderr pipes; asserts the immediate leader exit is reported as success with background_descendants and containment_empty true, that stdout/stderr capture is not marked Abandoned, that a TerminationRequested lifecycle event fires, and that containment reaches a terminal state.
- predicted: none · documented: none · derivable: yes · legible: most · trap: no
- note: The test name's 'leader_zero' and 'rejected' language did not map onto anything in the body I could identify from the name alone — it's really about a backgrounded orphan process holding inherited pipes after the tracked leader exits, not about a pid-0 sentinel.

### `bounded_capture_reports_truncation`
- spec 3 · read at `65ca9a754db4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:03:25Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Runs a script/command via the fixture that produces output exceeding the bounded capture buffer size, then asserts the captured result indicates truncation occurred (e.g. a truncated flag is set) while the captured bytes are capped at the configured limit rather than growing unbounded.
- found: Runs a shell pipeline producing 98304 bytes of output, collects it, and asserts the process succeeded, the captured stdout tail is exactly 64KB (the bound), and the output_capture status is explicitly Truncated.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `setup_identity_failure_occurs_before_user_code`
- spec 3 · read at `172fafd9669e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:23:07Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Configures the runscript containment harness with an identity (uid/gid) that will fail to apply (e.g., invalid/unprivileged user), runs it, and asserts the failure is reported as occurring during identity/setup rather than as if user code ran — likely checking that no user-code side effects (like expected output/marker file) were produced, proving the failure is caught before exec.
- found: Builds a fixture whose script would create a marker file if it ran, then corrupts runtime_identity.inode (simulating a planned-directory identity mismatch, e.g. TOCTOU/swap detection) rather than a uid/gid issue, runs the command, and asserts it errors with a \"planned directory\" message and the marker file was never created — proving the identity check fails before exec.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `durable_prepare_rejection_prevents_exec`
- spec 3 · read at `2acac270b202` · commit `1681528` · read by claude-sonnet-4.5 · asked for sonnet · via claude · when 2026-08-19T07:31:50Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A containment test verifying fail-closed behavior: when a "durable prepare" step (likely persisting some proof/state before exec, perhaps a lock or journal entry) is rejected/fails, the test asserts that the subsequent exec of the user script never actually occurs — probably checking no process was spawned or no side effects were observed.
- found: Test injects a failure via the event callback when a ContainmentPrepared lifecycle event fires (simulating a durable-journal write failure), asserting run_supervised returns that error and that the script's side-effect marker file was never created — confirming the prepared/ACK event gates actual exec.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `remote_host_recovery_never_signals_local_processes` — QUIRKY
- spec 3 · read at `9d2d9586590b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:16:01Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Test verifying that when recovery logic runs for a process that was executed on a remote host (not local), the containment/recovery code never sends a signal (e.g. kill) to a local PID — guarding against accidentally signaling an unrelated local process due to PID reuse/namespace mismatch across hosts. Likely sets up a fixture marked as remote, triggers the recovery path, and asserts no local signal/kill call occurred.
- found: Runs a real short script to completion, then mutates the resulting descriptor's host_identity to append \"-remote\" (simulating recovery being attempted from a different host than the one that ran it), calls recover_supervised, and asserts the outcome is ManualRecoveryRequired rather than any path that would act on/signal the process — since the process is presumed to belong to a different host's PID space.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Test name implies signal-safety but the actual mechanism is host_identity string mismatch driving a recovery-outcome classification, not an observed absence of a signal call.

### `forced_supervisor_fallback_is_explicit_and_operational`
- spec 3 · read at `e8052e2387fc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:33:35Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Forces the containment system to use the supervisor-based fallback strategy instead of cgroups (likely via an env var or config flag), runs a test command through it, and asserts both that this choice is explicitly recorded/reported (not silent) and that the supervisor fallback still successfully contains/terminates the process as expected.
- found: Sets command.containment_preference to ForceSupervisorFallback on a trivial "exit 0" fixture, runs it, and asserts the resulting containment backend is LinuxSubreaper, that a warning is present in the descriptor (making the fallback explicit), and that containment_empty is true (operational, nothing left behind).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `required_cgroup_either_arms_without_control_fd_leak_or_fails_before_user_code`
- spec 3 · read at `0e915cf02c19` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:11:46Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Exercises the "required cgroup" containment mode in both outcomes — success and failure. On success, it verifies the cgroup control file descriptor is properly armed and not leaked into the child/user process. On failure (cgroup setup fails), it verifies the failure occurs before any user code executes, so a required cgroup is never silently bypassed.
- found: Runs a shell script (as the "user code") that itself scans /proc/self/fd for any fd whose target mentions cgroup and exits 88 if found, otherwise writes a marker file; with containment_preference set to RequireLinuxCgroupV2, it asserts that either the run succeeds (LinuxCgroupV2 backend used, containment_empty true, marker written — meaning no leaked cgroup fd) or it fails with a cgroup-related error and the marker was never written (user code never ran).
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `timeout_contains_setsid_double_fork_and_term_ignoring_grandchild`
- spec 3 · read at `972714c8ed0a` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:40:04Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Test spawns a script that calls setsid and double-forks to detach a grandchild which explicitly ignores SIGTERM, then triggers a timeout in the containment/supervisor logic, and asserts (via assert_containment_terminal) that the grandchild is still reaped/killed despite the detachment and signal-ignoring — likely proving the supervisor falls back to SIGKILL or process-group-wide signaling rather than relying on SIGTERM propagation.
- found: Skips if setsid unavailable. Runs a shell fixture that traps SIGTERM at three nested levels and setsid-detaches a sleeping grandchild, with a 3s timeout budget (generously sized to survive containment/cgroup-fallback setup). Asserts the run timed out, containment ended empty, a ForcedTerminationRequested lifecycle event occurred, and containment reached a terminal state.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `wait_for_path`
- spec 3 · read at `94050060be6b` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T05:52:03Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Polls in a loop, sleeping briefly between checks, until `path.exists()` becomes true or the timeout duration elapses. If the timeout is exceeded without the path appearing, it panics with an assertion failure describing what it was waiting for.
- found: Loops checking path.exists() every 20ms until a deadline computed from timeout, returning early on success or panicking with a "timed out waiting for ..." message on expiry.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `application_crash_driver` — QUIRKY — TRAP
- spec 3 · read at `a43c5eb5ab8a` · commit `a6d7e33` · read by claude-sonnet-5 · via claude · when 2026-08-21T01:39:18Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A shared test helper (not itself a #[test]) that sets up a fixture, starts a long-running child process under the containment/supervisor mechanism, then simulates the driving application crashing (e.g., by exiting or killing the driver process abruptly without normal cleanup) rather than gracefully releasing containment. It likely returns some handle or state that calling tests (like application_crash_after_release_recovers_live_containment) use to assert that the child process/containment survives or is recoverable after the crash.
- found: This is a no-op unless TONEPOET_CRASH_DRIVER is set, meaning the test binary re-execs itself as a subprocess standing in for the 'driving' application. It reconstructs a SupervisedCommand entirely from env vars set by the parent test, runs it via run_supervised, and on lifecycle events (ContainmentPrepared, UserCodeReleased) writes marker files the parent test polls for. It ends in an unconditional panic, since the intended flow is for the parent test to kill this process externally at the right lifecycle point — normal return means the test setup/kill didn't happen as expected.
- predicted: some · documented: none · derivable: no · legible: most · trap: yes
- note: The function's outer shape (env-var-gated self-reexec entry point that panics on normal return) isn't guessable from the name/signature alone; a reader would assume it's a plain helper called in-process rather than a subprocess driver invoked via re-exec with TONEPOET_CRASH_* env vars.

### `application_crash_after_release_recovers_live_containment`
- spec 3 · read at `2d392fcf2148` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:37:36Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Uses application_crash_driver to simulate the supervising application crashing after it has already released/handed-off containment control (e.g. cgroup or control-fd) for a spawned child process. The test then verifies that containment can still be recovered live — e.g. by reattaching to the still-running child process or its process group — rather than losing track of it, and asserts the recovered containment reaches a correct terminal state.
- found: Spawns a separate driver subprocess (application_crash_driver) that sets up containment for a heartbeating shell script, writes a descriptor and signals it has released control; the test then SIGKILLs the driver to simulate an application crash, calls recover_supervised on the leaked descriptor, asserts the recovery outcome is ContainmentTerminated or ContainmentAlreadyEmpty, and verifies the heartbeat file stops growing after recovery — confirming no descendant process survived.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `vanished_or_pid_reused_supervisor_without_result_requires_manual_recovery`
- spec 3 · read at `2d3bff4ac31e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:33:57Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Sets up a fixture running a supervised process, then simulates the supervisor vanishing or its PID being reused (e.g. by killing it and/or spawning something else at the same PID) before any result/proof was recorded. Asserts that the recovery/containment logic, faced with this ambiguous state, refuses to auto-resolve and instead surfaces an error/status indicating manual recovery is required.
- found: Runs a real fixture command to completion, deletes the durable result.json to simulate a missing result, then mutates the descriptor's supervisor start_identity tick by +1 (modeling PID reuse via a differing-but-valid start tick rather than a malformed identity), calls recover_supervised, and asserts it classifies this as ManualRecoveryRequired.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

## tests/depth_format_matrix.rs

### the file itself
- spec 3 · read at `9f4eccbbcd12` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:15:59Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A large integration test file that exhaustively matrix-tests requested-vs-actual PCM depth/format combinations through the real (non-mocked) CUE-splitting pipeline using actual external encoder tools. It has substantial helper infrastructure (TempRoot for scratch dirs, executable_on_path/require_tools_or_skip to skip cells when an optional encoder isn't installed, create_sine to synthesize test audio, ffprobe/mp4-atom-parsing helpers to independently verify each output artifact's depth/format/tags/artwork) supporting a handful of #[test] functions that assert each supported depth/format cell publishes exactly the requested representation, that float32 CUE sources stay float32 through certain target formats (WAV/WavPack) while lossy sources default to integer PCM, and a "strict gate" test combining M4A freeform artwork with loudgain invariants — with unsupported combinations intentionally tested elsewhere (planning.rs) rather than here.
- found: Matches prediction well: real ffmpeg/wvunpack-driven matrix over 18 format/depth/tool-preference combinations, each run through run_pipeline_item and independently re-measured via ffprobe or wvunpack (WavPack needs an authoritative external decode since ffprobe can't be trusted for its depth), plus Source-target float32 preservation tests for plain WAV and WavPack CUE sources and a lossy-source-defaults-to-Int24 test. I underestimated the size/scope of the M4A strict-gate test, which independently exercises the Metadata stage, artwork/freeform-tag transfer bookkeeping, a second idempotent metadata pass, and loudgain ReplayGain tag injection via raw MP4 ilst atom parsing — well beyond a simple depth/format check.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: require_tools_or_skip has a real behavioral fork controlled by TONEPOET_REQUIRE_TOOLS: unset/0 silently skips missing-tool cases, non-empty/non-"0" panics — CI presumably sets it so this suite can't silently lose coverage, but a local run without it will happily report green with cells skipped.

### `new`
- spec 3 · read at `037720669fd7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:27:19Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Creates a fresh, uniquely-named temporary directory (using the label plus something like a process id or counter for uniqueness) on disk and returns a TempRoot wrapping its PathBuf, which Deref exposes and Drop cleans up on scope exit.
- found: Builds a unique path under std::env::temp_dir() using label + process id + nanosecond timestamp, wrapped in the tuple struct; it does NOT actually create the directory on disk here (that must happen elsewhere, e.g. via fs::create_dir_all before use).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The name TempRoot::new suggests directory creation but this function only constructs the path string; actual mkdir happens elsewhere (or on first write) — worth confirming where.

### `deref`
- spec 3 · read at `e318baac4da5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:52:05Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Standard Deref impl for TempRoot (a test RAII temp-directory wrapper), simply returning a reference to its inner path field (&self.0 or &self.path) so it can be used as a &Path/&PathBuf transparently in test code.
- found: Returns &self.0, the standard Deref boilerplate for the TempRoot tuple-struct wrapper.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `drop`
- spec 3 · read at `23b37d7b4307` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:12:17Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Drop impl for TempRoot — removes the temporary directory it wraps (via std::fs::remove_dir_all), likely ignoring/swallowing errors since it's a destructor, so tests don't leave temp dirs behind on disk.
- found: Removes the wrapped temp dir recursively; ignores NotFound errors but eprintln!s any other removal error rather than silently swallowing it.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `executable_on_path`
- spec 3 · read at `1f2772548c20` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:49:19Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Checks whether a binary named `name` exists somewhere on the system PATH, probably by iterating PATH env var directories and checking for a file (or using the `which` crate), returning true/false.
- found: Splits the PATH env var and checks if any directory contains a file with the given name; returns false if PATH is unset.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `require_tools_or_skip`
- spec 3 · read at `7ae417690edd` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:01:20Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Checks whether each tool name in `tools` is available on PATH (likely calling the `executable_on_path` peer for each), and if any are missing, prints/logs a skip message naming `test_name` and the missing tool(s), returning false so the caller can early-return and skip the test; returns true if all required tools are present.
- found: Checks each tool for presence on PATH via executable_on_path; if all present returns true. If any missing, checks TONEPOET_REQUIRE_TOOLS env var — if set to a truthy value (not \"0\"/empty), panics instead of skipping (so CI can force hard failure rather than silent skip); otherwise prints a skip message to stderr and returns false.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The TONEPOET_REQUIRE_TOOLS env-var escape hatch that turns missing tools into a panic rather than a skip wasn't guessable from the signature alone.

### `create_sine`
- spec 3 · read at `f2517e1d67a8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:49:48Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Shells out to ffmpeg using the "sine" lavfi source filter to synthesize a test tone of the given duration and sample rate, encoding it with the specified codec, and writes the result to path, panicking/asserting on failure — used to generate synthetic input audio fixtures for the depth/format test matrix.
- found: Runs ffmpeg with lavfi sine=frequency=1000:sample_rate=rate:duration=duration as input, encodes with -c:a codec to path, and asserts success with stderr in the panic message on failure.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `probe`
- spec 3 · read at `2afa0a260aad` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:27:13Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Runs ffprobe (via ffprobe_json) on the given file path to inspect the audio stream, extracts fields like sample format/bit depth, sample rate, and codec, and packages them into a Measurement struct; returns Err(String) with a descriptive message if ffprobe fails or expected fields are missing/unparseable.
- found: Runs ffprobe directly (not via the ffprobe_json helper) with -show_entries for codec_name/sample_fmt/bits_per_raw_sample/bits_per_sample on the audio stream, parses the key=value text output line by line, and returns a Measurement with codec, sample_fmt, and the two optional bit-depth fields (zero/unparseable filtered to None); errors on launch failure or nonzero exit status.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `authoritative_wavpack_depth`
- spec 3 · read at `fa74594f0a01` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:52:53Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Runs an external tool (likely `wvunpack -s` or similar) against the WavPack file at `path` to independently determine its actual encoded bit depth, parses the relevant field out of the tool's stdout, and returns Ok(PcmBitDepth) or Err(String) describing why it couldn't be determined — used to cross-check the depth the pipeline claims to have produced.
- found: Confirmed: runs `wvunpack -q -s`, scans combined stdout/stderr for a 'source:' line, hand-parses the digits before '-bit' and a float flag, and maps (bits, is_float) to the specific PcmBitDepth variant, erroring verbosely at each failure point.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `assert_measurement`
- spec 3 · read at `bfd3065c1dc2` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:23:49Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Given an output audio file and the requested PcmBitDepth, this probes the file's actual measured bit depth/format (likely via probe/ffprobe helpers), compares it against what was requested (possibly with format-specific exceptions, e.g. via authoritative_wavpack_depth), and returns Err(String) with a descriptive mismatch message if they don't match.
- found: Special-cases .wv files via authoritative_wavpack_depth; otherwise probes the file and, per requested PcmBitDepth (Float32/Float64/integer depths), checks the codec/sample_fmt string and bits_per_raw_sample/bits_per_sample against expectations, returning a descriptive Err on mismatch.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `base_request`
- spec 3 · read at `51d88c2a1338` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:09:15Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Constructs a default PipelineRequest wired to the given container path, output_root, and log_root, with baseline/neutral field values for everything else (format, depth, tagging options, etc.) so individual matrix test cells can clone it and override just the fields under test (e.g. target depth or format).
- found: Fully constructs a PipelineRequest literal with container/output_root/log_root wired in and every other field set to explicit baseline values (disabled stages, fail-on-collision naming, always-redo publish policy, all disc-source-selection fields None/default), for matrix tests to clone and override.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `run_checked`
- spec 3 · read at `bbf43d121a3d` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:17:25Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Spawns `tool` with `args` via std::process::Command, captures output, and panics with a message including stderr/exit status if the command fails or exits non-zero. Returns stdout as a String (trimmed or as-is) for the caller to parse.
- found: Runs the tool with args, panics if spawn fails or exit status is non-success (with stdout/stderr/args in the message), returns stdout as an owned String.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `create_single_flac_with_custom_tags_and_artwork`
- spec 3 · read at `99c525e98ea6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:57:41Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Calls create_sine to synthesize a FLAC audio file under root, then uses run_checked to shell out to metaflac (or similar) to set a handful of custom/unusual Vorbis comment tags and embed a cover-art picture block (e.g. via --import-picture-from), and returns the PathBuf to the created file. Likely also creates a temporary artwork image file to embed.
- found: Generates a small blue cover.jpg via ffmpeg lavfi, then generates a sine-wave FLAC via ffmpeg embedding that cover as attached_pic and setting several -metadata tags (TITLE, ARTIST, ALBUM, TRACKNUMBER, PRE_EMPHASIS, MY_NOTE) directly through ffmpeg rather than a separate create_sine + metaflac step.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ffprobe_json`
- spec 3 · read at `2ed7991b357e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:02:58Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Invokes the `ffprobe` CLI on the given path with flags like `-v quiet -print_format json -show_format -show_streams`, captures stdout, and parses it into a serde_json::Value, panicking (via expect/unwrap) if the command fails to run or the output isn't valid JSON.
- found: Runs ffprobe via the shared run_checked helper with -v error -show_streams -show_format -of json on the path, then parses the captured stdout into a serde_json::Value, expecting valid JSON.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `ffprobe_tag_map`
- spec 3 · read at `8bc567f1bb4b` · commit `1681528` · read by claude-sonnet-4.5 · asked for sonnet · via claude · when 2026-08-19T07:03:22Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Digs into the ffprobe JSON Value (likely probe["format"]["tags"], possibly merging stream-level tags too) and converts the tag key/value pairs into a BTreeMap<String, String> for deterministic comparison in test assertions.
- found: Merges format-level tags and audio-stream-level tags into one map, uppercasing keys (format tags take priority via insert, stream tags only fill gaps via or_insert_with) — matched the merging idea but missed the uppercase normalization and the format-wins-over-stream precedence detail.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `attached_picture_count`
- spec 3 · read at `92fb9f2db852` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:34:32Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Given an ffprobe JSON Value, iterates the "streams" array and counts entries whose disposition.attached_pic flag is 1 (or codec_type is "video"), returning that count as the number of embedded artwork/attached-picture streams.
- found: Filters ffprobe streams array to codec_type == video AND disposition.attached_pic == 1, counts them.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `read_be_u32`
- spec 3 · read at `9c3d704959c4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:18:18Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Reads 4 bytes from `bytes` starting at `offset` and interprets them as a big-endian u32, returning it as usize — a small helper for parsing binary container atoms (like MP4 box sizes) in these tests.
- found: Exactly as predicted: reads 4 bytes at offset, from_be_bytes into u32, cast to usize.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `read_be_u64`
- spec 3 · read at `62bfdca82420` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:32:23Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Reads 8 bytes from `bytes` starting at `offset` and interprets them as a big-endian u64, returning it as usize — used for parsing MP4/QuickTime atom sizes (the 64-bit extended size field) in test assertions alongside read_be_u32.
- found: Reads 8 bytes at offset as big-endian u64 and converts to usize, panicking (via expect) if the slice is short or the value doesn't fit — used for parsing MP4 box extended sizes in tests.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `count_mp4_ilst_atom`
- spec 3 · read at `6c560b4486c4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:37:17Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Walks MP4 box/atom structure in bytes from pos to end, reading each atom's 4-byte big-endian size and 4-byte fourcc type; recurses into container atoms (especially descending into 'ilst' with in_ilst=true), and increments a counter whenever an atom's fourcc matches target while in_ilst is true, returning the total count of matching atoms found nested inside the ilst container.
- found: Matches the general recursive walk I predicted, but only recurses selectively down the known moov/udta/meta/ilst path (not into every container atom), handles the MP4 64-bit extended-size (size32==1) and size-to-end (size32==0) special cases, and skips the 4-byte version/flags field when entering 'meta'.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `mp4_ilst_atom_count` — QUIRKY
- spec 3 · read at `2c730cc6e8e5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:23:57Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Opens the file at path, parses the MP4 box structure to locate the ilst metadata atom, then counts how many direct child atoms within ilst match the given 4-byte fourcc, using big-endian size reads to walk the atom tree; returns the count as usize.
- found: Reads the file into bytes and delegates entirely to count_mp4_ilst_atom(&bytes, 0, bytes.len(), false, atom); the actual atom-tree walking/counting logic lives in that other function, not here.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `assert_m4a_custom_artwork_state`
- spec 3 · read at `c751d27d9b2a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:10:42Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Checks the M4A/MP4 file's ilst atom for embedded cover artwork (via mp4_ilst_atom_count or count_mp4_ilst_atom) and asserts exactly one artwork atom is present — i.e. the custom artwork survived the pipeline pass without duplication or loss. It also builds and returns a BTreeMap of tag values (via ffprobe_tag_map) so the caller can make further assertions, with `pass` used to label the assertion failure messages (e.g. "before"/"after" conversion).
- found: Probes the file, asserts two specific freeform custom tags (PRE_EMPHASIS, MY_NOTE) survived, asserts exactly one attached-picture stream via ffprobe and exactly one covr atom via direct atom counting, using `pass` to label failure messages, and returns the tag map — I predicted the artwork-count and return-map/pass-label parts but missed the custom freeform tag checks entirely.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `strict_gate_exercises_single_file_m4a_freeform_artwork_and_loudgain_invariants` — QUIRKY
- spec 3 · read at `b1922314b408` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:14:04Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: An integration test that: skips if required tools (ffmpeg/encoder) are missing, creates a synthetic single-file source with custom tags and artwork, runs it through the real pipeline with "strict gate" enabled targeting M4A output, then verifies the produced M4A preserves the custom artwork (checking ilst/freeform atom counts) and has correct loudgain/ReplayGain measurements attached, asserting the strict gate doesn't drop or corrupt any of these invariants.
- found: Runs a single-file FLAC→ALAC(m4a) pipeline with custom tags/artwork, checks planner-reported satisfaction flags, runs the pipeline, checks artifact/publish path identity, then explicitly re-runs the metadata stage a second time on the published output to assert idempotent convergence of tags/artwork, then separately runs the ReplayGain (loudgain) stage afterward and asserts the four REPLAYGAIN_* tags are present and non-empty, verifying artwork survives all three passes.
- predicted: some · documented: none · derivable: no · legible: most · trap: no
- note: I expected a single conversion+verify test, but it actually re-applies metadata a second time to check idempotent convergence and runs ReplayGain as a distinct third pass rather than as part of the initial pipeline run — none of that staged-reapplication structure is visible from the name.

### `supported_pcm_depth_format_cells_publish_exact_requested_representation`
- spec 3 · read at `5c66bcf800b5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:39:39Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Iterates over a matrix of PCM depth/format combinations (e.g. 16/24/32-bit across WAV/FLAC/WavPack), skipping cells whose required tool isn't installed via require_tools_or_skip. For each remaining cell it builds a request with base_request, creates a sine test source, runs the pipeline through RealToolRunner, then probes the published output and asserts the measured depth/format exactly matches what was requested (using assert_measurement/probe), so no silent downsampling or format substitution happens.
- found: Builds a two-track CUE fixture from a 24-bit sine WAV, then for each of 18 (format, depth, preferred_tool) matrix cases checks per-case tool availability, runs the CUE pipeline via RealToolRunner with force_encode, expects exactly two published audio tracks, and asserts each one's measured bit depth matches the requested depth exactly; failures accumulate across all cases and are reported together at the end.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: The file-level doc explains the matrix's purpose but the per-cell tool-requirement and failure-accumulation mechanics live only in the body.

### `source_target_preserves_float32_cue_sample_class`
- spec 3 · read at `fcd046441668` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:55:12Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This async test creates a float32 PCM sine-wave source, runs it through the real CUE pipeline with a "source" target (i.e. requesting the source's own format/depth rather than an explicit conversion), and then probes the published artifact to assert the output sample format/class is still float32 rather than being silently downgraded to integer PCM. It likely uses require_tools_or_skip to gate on tool availability, create_sine and base_request to build the request, run_checked to execute, and probe/assert_measurement to verify the measured sample class matches float32.
- found: Builds a float32 PCM WAV source with a CUE sheet, runs it through the real ffmpeg pipeline requesting WAV output with BitDepthTarget::Source (i.e. preserve source depth) and force_encode true, then asserts exactly one published audio artifact whose measured PCM depth is Float32.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `lossy_cue_source_defaults_to_integer_pcm_for_flac_and_wav` — QUIRKY
- spec 3 · read at `c47599a88020` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:08:28Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: An async integration test that builds a CUE sheet pointing to a lossy source (e.g. an MP3), runs it through the real pipeline to produce FLAC and WAV targets without an explicit bit-depth override, and asserts (via probe/assert_measurement) that both outputs land on standard integer PCM (e.g. 16-bit) rather than inheriting/propagating any float sample class, since a lossy source has no meaningful native PCM depth to preserve. It likely calls require_tools_or_skip first to skip gracefully when the needed encoders aren't installed.
- found: Builds a two-track MP3 CUE sheet (with a measured tail track to account for encoder delay/padding), runs it through the real pipeline twice (FLAC and WAV, target_bit_depth=Source, force_encode=true), and asserts both published audio tracks in each case measure as Int24 PCM, collecting failures across both cases rather than failing fast.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `source_target_preserves_float32_wavpack_cue_sample_class` — QUIRKY
- spec 3 · read at `9e6f4272fdb9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:21:19Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: An async integration test that skips itself if required tools aren't present (require_tools_or_skip), creates a float32 sine-wave source with an associated CUE sheet, runs it through the real conversion pipeline (RealToolRunner) targeting WavPack output, then probes the resulting WavPack file(s) using authoritative_wavpack_depth/ffprobe and asserts the measured sample class/format is float32 (not silently downgraded to integer PCM), likely per CUE track.
- found: Encodes a float32 WavPack fixture via ffmpeg (fltp), verifies it's genuinely float32 with authoritative_wavpack_depth, wraps it in a CUE sheet, then runs it through run_pipeline_item with target_format=Wav and target_bit_depth=Source (i.e. WavPack source -> WAV target, not WavPack->WavPack as I guessed), and asserts the published WAV output measures as Float32 — verifying the float sample class survives a WavPack-sourced CUE through to a WAV target.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

## tests/dsd_reference_qualification.rs

### the file itself
- spec 3 · read at `75e5441f6320` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T16:07:02Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A large, tool-gated integration test suite (only active when TONEPOET_REQUIRE_TOOLS=1) that exercises the full P0 Reference DSD-to-PCM pipeline end-to-end against real SoX-ng and FFmpeg binaries rather than mocks. It builds real DSF/DFF/W64 fixtures, runs the actual planned decode/measurement/encode commands as subprocesses (with careful process supervision, timeouts, and output draining), asserts byte-exact decode results, header correctness, capacity/sparse-file behavior, gain and analyzer measurement correctness, and finally writes an atomic qualification report summarizing pass/fail for release automation.
- found: A ~7500-line gated integration/qualification test suite validating the P0 Reference DSD pathway end-to-end: cross-checks tonepoet's DSD decode/DST/PCM conversion pipeline against real SoX-ng and FFmpeg subprocesses across many rate/channel/depth/target cells (sample-exact hashes, W64/DSF/DFF header checks, true-peak/loudness measurement pipelines, gain-rounding bounds, sparse-file capacity contracts, metadata mutation). Inert unless TONEPOET_REQUIRE_TOOLS=1. Culminates in one enormous serde_json report literal documenting exact tool provenance (down to SoX source line references) and cell-contract pass/fail evidence, written atomically to a configurable path.
- predicted: most · documented: most · derivable: no · legible: not judged · trap: no

### `selected`
- spec 3 · read at `8c6f123d2239` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:51:18Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the TONEPOET_REQUIRE_TOOLS environment variable is set (e.g. to "1"), returning true if so, to gate whether the tool-dependent qualification test should actually run instead of being skipped/inert.
- found: Returns true iff the env var named by GATE constant equals "1", exactly as documented in the file doc's TONEPOET_REQUIRE_TOOLS gate description.
- predicted: full · documented: most · derivable: no · legible: full · trap: no

### `historical_v12_streamed_wav_capacity_contract_remains_frozen`
- spec 3 · read at `0ff7617d048a` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:55:09Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Asserts that some specific numeric constant or threshold (e.g. a max file size / sample count boundary that determines when streamed WAV output switches format, historically tied to a "v12" tool version's behavior) still equals its known frozen value. This guards against an accidental regression in capacity/size-limit logic that must stay byte-for-byte compatible with an older reference implementation, and doesn't require actual external tools to run.
- found: Asserts three frozen constants: a ReferenceStreamedWavCapacityEvidenceV2 expected_transition_count() equals 10, its STREAM_HEADER_BYTES equals 66, and a JSON identity blob's "policy" field equals the literal string "sox_ng_14_8_0_1_v12" (pinning both the numeric capacity contract and the exact external tool version/policy string it was validated against).
- predicted: most · documented: none · derivable: no · legible: full · trap: no
- note: The policy string pins an exact SoX-ng build version, not just a capacity number — a tool upgrade would need this test updated deliberately, not just the numeric contract.

### `required_tool`
- spec 3 · read at `88e1388d6635` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T05:55:20Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Reads the environment variable named by `variable`, panicking/expecting with a clear message if it is not set (since this is a tool-gated release qualification test), and returns the value as a PathBuf. May also assert the path exists on disk.
- found: Reads the named env var, panics with a message naming the variable if unset, then canonicalizes the path (panicking with the error if that fails) and returns the resolved PathBuf.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `production_metadata_runner`
- spec 3 · read at `9398211437cb` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T11:50:32Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Constructs and returns a RealToolRunner struct literal populated with the four given tool paths (ffmpeg, metaflac, wvtag, atomic_parsley), used so the qualification test invokes real external binaries for metadata writing rather than mocked ones.
- found: Builds a RealToolRunner via a HashMap mapping tool names (ffmpeg, metaflac, wvtag, AtomicParsley) to their paths, exactly as predicted, though the exact constructor shape (HashMap-based ::new) was a detail not fully anticipated.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `qualification_metadata`
- spec 3 · read at `1fb73e984237` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:45:35Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Test-fixture helper that constructs and returns a fixed, hardcoded pair of TrackMetadata and AlbumMetadata (e.g. sample artist/title/album/track number) used to tag output during the DSD reference qualification pipeline runs, giving the tests in this file consistent, deterministic metadata to check against.
- found: Builds a hardcoded TrackMetadata/AlbumMetadata pair with nearly every field populated (title, artist variants, composer, performer, arranger, genre, date, track/disc numbers, ISRC, publisher, copyright, comment, pre_emphasis, plus extra maps with MY_NOTE and CATALOG keys) to exercise the full production metadata path in the DSD reference qualification test.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `w64_planner_request` — QUIRKY
- spec 3 · read at `cc899e09412b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:03:15Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Builds a PipelineRequest for a DSD-to-W64 (Wave64) reference conversion: sets target_format to W64, target sample rate and bit depth to the given sample_rate_hz/depth, wires input/output/work paths under `root`, and configures the qualified/production tool paths (SoX-ng, FFmpeg) needed for the release qualification test, for use alongside w64_planner_track to plan and validate the conversion.
- found: Constructs a fully-populated PipelineRequest struct literal (dozens of fields: source options, naming, publish, log, stage policies, etc.) for converting a DSD .dsf source under root to a .w64 container, with target format Wav, given sample rate and bit depth, and native_v2 DSD settings.
- predicted: some · documented: none · derivable: no · legible: full · trap: no

### `w64_planner_track` — QUIRKY
- spec 3 · read at `113bd358d40b` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:24:50Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Constructs and returns a PreparedTrack describing a W64 (Sony Wave64) output track for the planner, likely setting the input path, channel count, and W64-specific container/format fields (e.g. codec = W64, some default sample/bit depth), analogous to sibling helpers like w64_planner_request. Probably just builds a small struct literal with minimal logic, no I/O.
- found: Builds a fixture PreparedTrack for a DSD source fed through the W64 planner path: fixed TrackId (ordinal 1, track 1), source_ref pointing at the staged input file, metadata from qualification_metadata(), a hardcoded expected_samples of 262144 and DSD64 sample rate (2,822,400 Hz), a SourceAudioDescriptor tagged as Dsd, no bit_depth, and a mono-fixture warning appended only when channels == 1.
- predicted: some · documented: none · derivable: no · legible: full · trap: no

### `required_sibling_tool`
- spec 3 · read at `bbb3c7773304` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:16:04Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Given a path to one required tool binary, computes the path to a sibling executable (same parent directory) with the given `executable` name, for locating other flake-owned tools relative to a known one. Likely asserts/panics if that sibling file doesn't exist, since this is release-qualification test scaffolding that must fail loudly when the toolchain isn't as expected.
- found: Joins the tool's parent dir with the executable name, panicking if tool has no parent, then canonicalizes the candidate path, panicking with a detailed message if that fails.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `apply_qualified_environment` — OBSCURE
- spec 3 · read at `884aa735425a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:58:06Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Sets environment variables on the given Command to point at the flake-owned SoX-ng and FFmpeg binaries (release-qualified tool paths), so the spawned process uses those specific tool versions instead of whatever is on the ambient PATH.
- found: Clears the command's entire environment and sets only LC_ALL=C, ensuring a deterministic locale-free environment for the qualification test rather than pointing at specific tool paths.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: Name suggests tool-path qualification but the function is actually about environment hygiene (clean env + fixed locale) — the qualified tool paths must be set elsewhere (likely via PATH before env_clear or passed separately).

### `finish`
- spec 3 · read at `47b620621e3a` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:58:26Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Consumes the OutputDrain (which was reading a child process's stdout/stderr on background threads), joins those threads to collect the accumulated output bytes, and returns the collected bytes along with an optional error/status string (e.g. a thread-join failure message or an overflow/timeout note), signaling whether draining completed cleanly.
- found: Waits on a completion channel with a timeout for the drain result, joins the background task if the channel result was Ok (reporting a panic message if the join fails), then returns the locked/cloned tail buffer plus an optional error string from either the timeout, the inner result error, or the join panic.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `drain_child_output`
- spec 3 · read at `ac2ece327cff` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:34:52Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Spawns a background thread that reads from the given stream (child process stdout/stderr) in a loop, accumulating the bytes into a shared buffer (e.g. Arc<Mutex<Vec<u8>>>) and possibly echoing lines prefixed with `label` to the test's own output for visibility, returning an OutputDrain handle holding the thread's join handle and the shared buffer so the caller can retrieve accumulated output later via OutputDrain::finish.
- found: Spawns a thread reading the stream into a fixed-capacity Arc<Mutex<Vec<u8>>> tail buffer, trimming from the front once it exceeds QUALIFICATION_RETAINED_TAIL_BYTES (keeping only the last N bytes rather than the full output as I predicted), and sends a completion Result over an mpsc channel; returns an OutputDrain with label, tail, completion receiver, and join handle.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `terminate_and_reap_result`
- spec 3 · read at `bf2f03a3fc0e` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:19:16Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Attempts to terminate the given child process (kill/SIGTERM) and then wait() on it to reap the exit status, returning Ok(ExitStatus) on success or Err(String) with a message including `label` if killing or waiting fails. Likely called by a `terminate_and_reap` sibling that panics/unwraps this Result for convenience in test cleanup.
- found: First tries a non-blocking try_wait to see if the child already exited; if not, kills it, then polls try_wait in a loop with sleeps until a deadline (QUALIFICATION_TERMINATION_TIMEOUT), returning Ok(status) as soon as it reaps, or Err with a message combining label, timeout, kill error and last wait error if it never reaps in time.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `terminate_and_reap`
- spec 3 · read at `5fb480a15794` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:06:31Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Kills the given child process and waits on it to reap it, using `label` for an error message if kill or wait fails, returning the resulting ExitStatus — most likely a thin wrapper that calls the sibling terminate_and_reap_result and unwraps/expects on it.
- found: Thin wrapper calling terminate_and_reap_result and panicking with the error message on failure.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `wait_with_deadline`
- spec 3 · read at `cdeb64acf702` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:07:10Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Polls the child process in a loop using try_wait(), sleeping briefly between checks, tracking elapsed time via Instant::now() against the timeout Duration. Returns Ok(ExitStatus) if the process exits within the deadline, or Err(String) with a message referencing `label` if the timeout is exceeded (possibly killing the child on timeout).
- found: Polls child.try_wait() in a loop; on Some(status) returns Ok. On None, sleeps if before the deadline, otherwise terminates/reaps the child and returns a formatted Err describing the timeout. On an Err from try_wait itself, also terminates/reaps and returns a formatted Err describing the inspection failure.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `run_configured_command_unchecked` — QUIRKY
- spec 3 · read at `96f09dafe782` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:35:25Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Builds a std::process::Command for the binary at path with args, calls the configure_environment closure to let the caller customize env vars, then runs it (likely via .output()) and returns the raw Output — without checking exit status success (unlike the sibling run_configured_command), panicking only if spawning itself fails.
- found: Spawns the command with piped stdout/stderr, drains both concurrently via background tasks while waiting on the child with a deadline/timeout, panics with rich diagnostics on spawn failure, wait failure, or drain error, and returns the raw Output (status not checked for success — that's left to the caller, hence \"unchecked\").
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `run_configured_command`
- spec 3 · read at `adb1c670525c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:04:26Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Builds a std::process::Command from `path` and `args`, calls `configure_environment` to let the caller customize env vars/stdio, runs it, and asserts (panics with combined stdout/stderr on failure) that it exited successfully, then returns the captured Output. Distinguished from the sibling `run_configured_command_unchecked`, which presumably skips the success assertion.
- found: Delegates entirely to run_configured_command_unchecked, then asserts the exit status was success, panicking with path/args/stdout/stderr if not, and returns the Output.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `run_with_pre_clear_environment` — QUIRKY
- spec 3 · read at `78ec28a98c94` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:25:39Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Builds a std::process::Command for the given path/args, calls env_clear() to wipe the inherited environment, then sets each key/value pair from pre_clear_environment before spawning and capturing output() — used to test that subprocess environment isolation works regardless of what's in the parent environment. Panics/expects on spawn failure and returns the Output.
- found: Delegates to run_configured_command with a closure that sets each pre_clear_environment var then calls apply_qualified_environment(command) — I predicted manual env_clear/spawn logic rather than this delegation pattern, though the overall intent (set vars before the real qualified env is applied) matched.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `run_planned_legacy_command` — QUIRKY
- spec 3 · read at `b2f7072ab9af` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:11:25Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Builds a std::process::Command from path plus planned's args (and maybe env), runs it synchronously via output(), and panics/expects with a descriptive message on spawn failure or nonzero exit, returning the captured Output.
- found: Delegates to run_configured_command with path and planned.args, using a closure to apply the environment policy (clear or inherit) and set planned.environment vars on the Command before it runs; doesn't do its own success/failure checking here.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `run` — OBSCURE
- spec 3 · read at `edbb53869079` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:13:37Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A small test helper that spawns std::process::Command::new(path).args(args), captures stdout/stderr via .output(), and unwraps/expects success, returning the Output struct for callers to assert on. Likely panics with a descriptive message if the process fails to spawn.
- found: Delegates to run_with_pre_clear_environment(path, args, &[]) — a thin wrapper with no vars to clear, not a direct Command invocation.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no

### `run_unchecked` — QUIRKY
- spec 3 · read at `42abb2cf9f63` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:56:47Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Spawns a subprocess at `path` with `args` (likely via std::process::Command::new(path).args(args).output()), capturing stdout/stderr, waits for completion, and returns the raw Output struct without checking/asserting the exit status — leaving status-checking to callers, unlike a "checked" variant elsewhere in the file.
- found: Thin one-line delegation to run_configured_command_unchecked(path, args, apply_qualified_environment) — it doesn't spawn the process itself, it just supplies the qualified-environment configuration hook to a shared implementation.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `combined`
- spec 3 · read at `9c9f29e36c2a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:46:21Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Test helper: combines a subprocess Output's stdout and stderr into a single String (likely lossy-converted via String::from_utf8_lossy and concatenated with a separator) for easy assertion/inspection in test failure messages.
- found: Formats stdout and stderr (each lossily decoded) joined by a newline, exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `first_nonempty_line`
- spec 3 · read at `f3696143d04b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:51:08Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Iterates lines of text, trims each, and returns the first line that isn't empty after trimming — likely returning an empty string if none are found, probably used to grab the first meaningful line of subprocess output/error text.
- found: Exactly as predicted: trims each line, finds the first non-empty one, defaults to empty string if none.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `qualified_environment_probe_child`
- spec 3 · read at `508e7d32fdd6` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T05:50:16Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: The child-process-side helper invoked by qualify_subprocess_environment_isolation: it runs in a spawned subprocess and probes/prints its environment (e.g., specific env vars) to stdout so the parent test can verify environment isolation/clearing worked correctly.
- found: Prints TONEPOET_QUALIFICATION_AMBIENT_POISON and LC_ALL env var values (or "unset") so the parent test can verify subprocess environment isolation.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `qualify_subprocess_environment_isolation`
- spec 3 · read at `8d1bafc62cc6` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T22:16:36Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Spawns a probe child process using run_with_pre_clear_environment with a deliberately set/polluting environment variable, asserts that the child process does not observe that variable (confirming environment isolation before invoking SoX-ng/FFmpeg), and returns a JSON Value summarizing the pass/fail result for aggregation into the qualification report.
- found: Re-invokes its own test binary (via current_exe) to run the qualified_environment_probe_child test as a child process, using run_with_pre_clear_environment to clear ambient env and inject a poison variable plus (implicitly) LC_ALL=C. Asserts the child's combined output shows the poison var was NOT observed and locale was normalized to C, then returns a structured JSON pass report for the qualification suite.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: File doc explains the overall gated qualification suite but nothing about this specific isolation mechanism (re-exec of the test binary as its own probe child).

### `sha256_hex`
- spec 3 · read at `311da53f8e5c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:22:19Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Computes the SHA-256 digest of bytes (via the sha2 crate) and formats it as a lowercase hex string, used for verifying fixture/output integrity in this qualification test suite.
- found: Sha256::digest(bytes) formatted as lowercase hex via format!("{:x}", ...), exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `canonical_fixture_corpus_digest`
- spec 3 · read at `5b302f1559f3` · commit `1681528` · read by claude-sonnet-5 · when 2026-08-19T08:33:55Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Computes a single stable digest over a set of named fixture files by sorting them by name (for determinism regardless of input order), then hashing each name and its byte content together (likely via the sha256_hex helper) into one combined hash, returning the hex string. Used to fingerprint the reference DSD fixture corpus so qualification results can be tied to a specific corpus version.
- found: Sorts the (name, bytes) pairs by name, then feeds a versioned domain-separation prefix ("sacd-rs-dst-reference-fixtures/v2\0") into a Sha256 hasher, followed by each name and byte content length-prefixed (u64 big-endian) to avoid ambiguity, and returns the hex digest.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The versioned prefix string and length-prefixing scheme aren't guessable from the name alone but are standard hash-domain-separation practice.

### `json_u64` — QUIRKY
- spec 3 · read at `5484c7d3dc9d` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:29:50Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Looks up `field` in the JSON `value` object and extracts it as a u64, panicking/expecting with a helpful message if the field is missing or not representable as u64 — essentially value[field].as_u64().expect(...).
- found: Extracts value as u64 directly, or if it's a string, parses it as u64 (handles ffprobe emitting numbers as JSON strings), panicking with a message naming the field and value if neither works.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `optional_json_u64` — QUIRKY
- spec 3 · read at `caeda07d0ab3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:25:08Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Helper that extracts a u64 from an optional JSON value: looks up `field` on `value` if present, converts to u64, and defaults to 0 if the value is None, the field is missing, or it isn't a valid u64 — contrasted with a sibling `json_u64` that presumably panics/asserts when the field is required and missing.
- found: Returns 0 if value is None or Value::Null; otherwise delegates to json_u64(value, field) to extract and presumably assert the field's u64 value.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `read_le_u64`
- spec 3 · read at `9a5372b03152` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:49:57Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: read_le_u64 reads the first 8 bytes of `bytes` as a little-endian u64 (via u64::from_le_bytes), panicking/asserting with a message including `label` if the slice is shorter than 8 bytes — a small helper used for parsing binary header fields (like W64/DSF header sizes) in these qualification tests.
- found: Converts the byte slice to a [u8;8] via try_into and decodes it as little-endian u64, panicking with a label-including message if the slice isn't exactly 8 bytes.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `inspect_w64_header` — QUIRKY
- spec 3 · read at `4ba0c39a589a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:18:01Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Opens the file at input, reads the Wave64 header (GUID-based chunk IDs and 64-bit chunk sizes, unlike RIFF's 32-bit), locates the fmt and data chunks, and returns a W64HeaderObservation with fields like data chunk size/offset, channel count, sample rate, and bits per sample, using read_le_u64 to parse the 64-bit size fields.
- found: Reads the file bytes, asserts the first 16 bytes match the W64 RIFF GUID, scans byte-windows to locate the W64 data-chunk GUID (rather than assuming a fixed offset), then reads the little-endian 64-bit RIFF size field and data-chunk size field, computes the payload offset (data_chunk_offset + 24) and remaining payload bytes present, and packs it all into a W64HeaderObservation — with no fmt/channel/sample-rate parsing at all.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `sox_info_value`
- spec 3 · read at `d08c50b24d8c` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T21:22:59Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Runs the sox binary as a subprocess with an --info-style flag (e.g. -r, -c) against the input file to query one piece of audio info, captures and trims stdout, and returns it as a String — expecting/panicking on subprocess failure since this is release-qualification test infrastructure.
- found: Runs sox --i <flag> <input> via a helper `run`, decodes stdout as UTF-8 (panicking with a descriptive message if not), trims it, and returns the value.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `sox_reported_sample_frames`
- spec 3 · read at `a01c2e3f42e2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:03:01Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Runs the given sox binary against the input file (likely via a "-n stat" or "--info"/soxi style invocation) to get its reported sample frame count, parses the numeric text output (possibly using the sox_info_value helper) into a u64, and returns it. Probably panics/unwraps on parse failure since this is test qualification code.
- found: Calls sox_info_value with the "-s" flag to get sample count as text, parses to u64, panics with a descriptive message including the input path if parsing fails.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `assert_exact_w64_package_probe`
- spec 3 · read at `f47b911043dd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:48:02Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Runs sox and ffprobe as subprocesses against the input W64 file, parses their reported header/stream info, and asserts depth, sample_rate_hz, channels, and expected_frames all match exactly what was passed in, likely also cross-checking sox vs ffprobe agreement.
- found: Maps depth string to expected bits/encoding, validates the W64 file structure directly (in-process authority check), then cross-checks sox's reported type/rate/channels/depth/encoding/frames, then cross-checks ffprobe's JSON output for codec/rate/channels/duration/format, and finally runs a full ffmpeg traversal to confirm it also accepts the file.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `assert_exact_package_probe` — QUIRKY
- spec 3 · read at `d5b3d8f9bc4a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T19:36:25Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Invokes the sox binary to convert/package input to the given target format at depth/sample_rate_hz/channels, producing an output file. Then uses ffprobe (and/or sox_info_value/sox_reported_sample_frames as a second source of truth) to inspect that output's actual sample rate, channel count, and bit depth, asserting each exactly matches the requested parameters, and if expected_frames is Some, asserts the decoded/reported frame count matches exactly too — panicking with a descriptive message on any mismatch.
- found: For target=='wav_w64' delegates entirely to assert_exact_w64_package_probe (using sox) with expected_frames required. Otherwise it never touches sox at all — it runs ffprobe directly on the already-produced `input` file, parses the JSON stream/format info, and asserts codec_name, sample_rate, channels, and effective bit depth (bits_per_raw_sample falling back to bits_per_sample) against a hardcoded target/depth lookup table, checks sample_fmt is float vs integer as expected, and checks the container format_name matches — expected_frames is unused outside the w64 branch.
- predicted: some · documented: none · derivable: no · legible: most · trap: no
- note: The `sox` and `expected_frames` parameters are dead weight for every non-'wav_w64' target — only the ffprobe path runs, and the input file is assumed already packaged rather than being produced by this function.

### `r64_decode_authority` — QUIRKY
- spec 3 · read at `5556de8e5212` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:27:40Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Given a FinalPcmContract describing the final PCM's properties, returns which external tool (SoX-ng vs FFmpeg) is the "authority" for decoding R64-format samples — a simple match/branch on contract fields returning a ReferenceDecodeAuthority enum variant, paralleling sibling functions like qpcm_decode_authority and packaged_decode_authority for other format families.
- found: Delegates to tonepoet_pipeline::reference_decode_authority with role ReconstructionR64W64, forcing sample_kind/bit_depth to Float64 and dither to None while keeping the caller's sample_rate and channels, then unwraps the result (expect) rather than branching manually.
- predicted: some · documented: none · derivable: no · legible: full · trap: no

### `qpcm_decode_authority` — QUIRKY
- spec 3 · read at `7190330e1f15` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:35:41Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Given a FinalPcmContract describing the expected output format for the "qpcm" pathway, returns which external reference tool (ffmpeg or sox) should be treated as authoritative for decoding/verifying that contract's output, likely by matching on fields of the contract (sample rate, bit depth, channel count) similar to the sibling r64_decode_authority function.
- found: Thin wrapper that calls the shared tonepoet_pipeline::reference_decode_authority with a role tag (TerminalQpcmW64) and the contract, unwrapping the Option with an expect — no local matching logic at all.
- predicted: some · documented: none · derivable: no · legible: full · trap: no

### `packaged_decode_authority` — QUIRKY
- spec 3 · read at `0d1483b2180c` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T10:19:09Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A dispatcher that, given the resolved output target and final PCM contract, decides which decode authority (e.g. ffmpeg, sox streamed, or a specific fixture-based route like r64/qpcm) should be treated as ground truth for verifying the packaged output — likely branching on target/contract fields and delegating to one of the sibling *_decode_authority helpers, returning a ReferenceDecodeAuthority value.
- found: Thin test-helper wrapper: constructs a ReferenceDecodedSampleRole::PackagedOutput{target} and delegates to tonepoet_pipeline::reference_decode_authority(role, contract), unwrapping with expect — no branching or dispatch logic lives in this function itself, it's all in the pipeline crate.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `post_metadata_decode_authority` — QUIRKY
- spec 3 · read at `30e7afcd6959` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T11:45:12Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Dispatches on the resolved output target/contract to pick which decode-authority strategy applies after metadata has been written to the file — delegating to one of the sibling helpers (packaged_decode_authority, r64_decode_authority, qpcm_decode_authority) depending on the target's packaging format, and returns the resulting ReferenceDecodeAuthority used to verify the decoded PCM matches expectations post-metadata-write.
- found: Calls the pipeline library's reference_decode_authority with a PostMetadataOutput role tagging the target, plus the contract, and unwraps with an expect — it's a thin typed wrapper around a shared library function, not a local dispatch among the sibling helpers.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The sibling names (r64_decode_authority, qpcm_decode_authority, packaged_decode_authority) are separate standalone functions, not branches this one dispatches to.

### `assert_qualification_decode_route_table` — QUIRKY — TANGLED
- spec 3 · read at `e80384a5fcf9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:21:26Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A qualification-test function that iterates over the known DSD decode routes (r64, qpcm, packaged, post-metadata), calling each *_decode_authority helper to determine which decode path/tool is authoritative for that route, and asserts these match expected values. It builds and returns a serde_json::Value summarizing the route table as evidence for the qualification report.
- found: Asserts the reference decode route rule count, checks each *_decode_authority function returns the correct decode mechanism/hash encoding for int24/float32/float64 PCM contracts, verifies that DirectFfmpeg is rejected (with a specific error message) for every Float64 W64 role that requires the sox raw-stream route, runs a carrier-binding mislabeling regression (QPCM W64 impersonating a packaged RIFF output must be rejected before command construction), then returns a serde_json::Value summarizing all of this as qualification evidence.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no
- note: The file-level doc explains the overall gating purpose but says nothing about this specific function's route-table/negative-path/carrier-mislabeling checks.

### `qualification_decode_route_table_evidence` — QUIRKY
- spec 3 · read at `fe730489aa8c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:44:42Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Builds and returns a serde_json::Value documenting the "decode route table" — a structured description of which decode authority (r64, qpcm, packaged, post-metadata) is used for each DSD/format route, likely referencing the various *_decode_authority peer functions. Serves as evidence/reporting data rather than performing assertions itself, possibly consumed by assert_qualification_decode_route_table or dumped to a qualification report.
- found: Iterates a REFERENCE_DECODE_ROUTE_RULES table, building a serde_json Map keyed by "role_class:hash_encoding" with each value holding bit_depth/mechanism/hash_encoding, asserting no duplicate keys and that the map size matches the rule count, then returns it as a Value::Object — evidence data for the qualification test suite.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The file_doc describes the whole qualification test file/gating mechanism, not this specific function's JSON structure.

### `ffmpeg_sample_hash`
- spec 3 · read at `9ff343804719` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:37:47Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Spawns ffmpeg as a subprocess to decode `input` into raw PCM samples using the given `pcm_codec`, writing to stdout (piped, e.g. via `-f` raw format to `-`). It then hashes the resulting PCM byte stream (likely SHA-256) and returns the hex digest string, used to compare decode output for equality/regression against other decode authorities in the qualification suite.
- found: Runs ffmpeg with specific flags to decode the audio stream (dropping metadata/video/subs/data streams) with the given pcm_codec, using ffmpeg's own `-f hash -hash sha256` muxer to produce a hash directly rather than piping raw PCM to an external hasher. Parses stdout/stderr combined output for a line starting with "SHA256=" and returns that value, panicking if not found.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `decoded_sample_hash`
- spec 3 · read at `f1d9ef7ff875` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:33:51Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Dispatches on the ReferenceDecodedCarrier variant and delegates to the matching helper (e.g. ffmpeg_sample_hash or a sox-based hash function), passing the sox/ffmpeg tool paths, to decode the carrier's PCM samples and return a hex digest string used to compare different decode routes for bit-exact qualification.
- found: Gets the carrier's decode authority and contract, then matches on the authority's decode mechanism (DirectFfmpeg vs SoxFloat64W64RawStream) to delegate to ffmpeg_sample_hash or sox_streamed_float64_w64_sample_hash respectively, asserting the sox route's hash encoding is Float64Le before calling it.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `sox_streamed_float64_w64_sample_hash`
- spec 3 · read at `24d9dd0a6553` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:09:35Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Shells out to `sox` to convert/stream the input file into float64 W64 PCM at the given sample rate and channel count, likely piping output rather than materializing a full intermediate file, then hashes the resulting sample data (possibly decoding via ffmpeg or reading the W64 payload directly) and returns the hash as a string for comparison against other decode-authority hashes in the qualification suite.
- found: Spawns sox to stream raw float64 little-endian PCM to stdout, pipes that directly into ffmpeg's stdin which decodes it as f64le at the given rate/channels and re-encodes via ffmpeg's built-in hash muxer (-f hash -hash sha256) to compute a SHA256 of the sample data, with careful child-process supervision/stderr draining, then parses the SHA256= line from ffmpeg's output.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: Uses ffmpeg's own hash muxer rather than an external hash step, and pipes sox stdout straight into ffmpeg stdin — no intermediate hashing function.

### `synth_r64_fixture` — QUIRKY
- spec 3 · read at `e10177603df6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T15:31:25Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Shells out to the given `sox` binary via Command with `-n` and `synth`/`sine` (or `silence` if `silence` is true) style arguments to generate a raw R64/DSD64-rate audio fixture at sample_rate_hz/channels/amplitude, writes it to `output`, and asserts the command exits successfully (panicking otherwise).
- found: Thin wrapper delegating to synth_r64_fixture_duration with a fixed duration of \"0.05\" seconds, passing through all other args unchanged.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The actual sox invocation logic lives in synth_r64_fixture_duration, not here — this is just a fixed-duration convenience wrapper.

### `synth_r64_fixture_duration` — QUIRKY
- spec 3 · read at `1b501bd8da6d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T10:55:41Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Builds and runs a `sox` command to synthesize an r64 (DSD) fixture at `output`: uses `-n synth <duration_seconds>` with either `sine` or `silence` depending on the `silence` flag, applies the given amplitude/sample_rate/channels, and asserts the process succeeded (panicking with output on failure).
- found: Builds a sox command that always outputs w64/floating-point/64-bit (despite the r64 name), using `trim 0 <duration>` on a null source for silence, or `synth <duration> sine 997 vol <amplitude>` for tone, then delegates to a shared `run` helper.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Function name says r64 but the actual output type/encoding written is w64 floating-point 64-bit — the format mismatch between name and -t w64 arg could confuse future readers.

### `probe_direct_ffmpeg_f64_w64`
- spec 3 · read at `8e004dae8319` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:43:58Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Constructs and runs an ffmpeg Command directly (bypassing the pipeline abstraction) against the given W64/float64 input file to probe/decode it, returning the raw process Output (stdout/stderr/status) for the test to inspect independently of the app's own ffmpeg invocation.
- found: Builds an ffmpeg arg list that decodes the W64 input's first audio stream directly to raw float64le PCM on stdout (dropping video/subs/data streams, quiet logging), then runs it via the shared run_unchecked helper and returns the Output.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `encode_float64_w64_fixture`
- spec 3 · read at `3f39b4150523` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:56:42Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Writes the given f64 samples to a temporary raw file, then shells out to the sox binary to encode that raw float64 data into a Wave64 (.w64) file at root/name with the given sample rate, returning the path to the resulting fixture file.
- found: Writes raw little-endian f64 samples to a temp file, then invokes sox to convert that raw float64 mono stream at the given sample rate into a 64-bit floating-point Wave64 (.w64) file, returning the output path.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `exact_float64_w64_header` — OBSCURE
- spec 3 · read at `2d8eb7612de3` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:10:15Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Takes a parsed W64 header observation and returns true only if every field exactly matches what's expected for a 64-bit IEEE-float W64 file (format tag/GUID, bits-per-sample=64, block alignment, etc.), used as a strict integrity assertion in the qualification test.
- found: Checks that the RIFF size field equals the actual file byte count, and the data chunk size field equals payload bytes plus the 24-byte W64 chunk header, asserting exact size-field correctness rather than format/bit-depth fields.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no

### `encode_w64_characterization_fixture`
- spec 3 · read at `4b9786b7d0db` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:19:45Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Writes `samples` as raw PCM to a temp file, then invokes the given `sox` binary to convert/encode that raw data into a Wave64 (.w64) file at `root/name...` with the specified sample_rate_hz, channels, and depth (bit depth/format string), asserting the sox command succeeds, and returns the PathBuf of the produced .w64 fixture for later characterization/hashing in the qualification tests.
- found: Writes samples as raw f64le to a temp file, then runs sox to convert that raw float64 data into a .w64 file with target depth (int24/float32/float64 mapped to sox encoding+bits), sample rate, and channel count, plus an explicit 'gain 0' pass to exercise the same signed-Q1.31 effects boundary as production gain code, returning the output path.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The 'gain 0' no-op is deliberate — it forces sox through its effects-chain fixed-point boundary to match production behavior, not a leftover no-op call.

### `w64_payload_is_all_zero`
- spec 3 · read at `9bd7f0325808` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:35:09Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Opens the W64 file at `path`, parses/skips its RIFF64-style header to locate the data chunk, and scans the sample bytes to check whether every byte is zero (i.e., the audio is pure silence). Returns true if so, false if any nonzero byte is found — likely used as a sanity guard against degenerate all-silence fixtures/output in the qualification tests.
- found: Delegates header parsing to inspect_w64_header to get the payload offset, reads the whole file, and checks that every byte from that offset onward is zero.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `probe_ffmpeg_w64_full_traversal`
- spec 3 · read at `6ac2c4b08a22` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:46:39Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Builds and runs an ffmpeg Command against the input W64 file with args that force full decoding/traversal of the file (e.g. -i input -f null -), discarding actual output, and returns the process Output (status/stdout/stderr) so the caller can assert ffmpeg read the whole file without error.
- found: Runs ffmpeg via run_unchecked with flags forcing strict full decode of the audio stream (-xerror, -map 0:a:0, disabling video/subs/data) into a null muxer, returning the raw Output for the caller to inspect.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `decode_w64_to_f64`
- spec 3 · read at `593c0ab09624` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:13:23Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Spawns ffmpeg as a subprocess to decode the given W64 file to raw float64 PCM (piped to stdout), reads the raw bytes, converts them into a Vec<f64>, and checks/asserts the result has expected_values samples.
- found: Runs ffmpeg to decode a single audio stream from the W64 input to raw pcm_f64le on stdout, converts the byte output into a Vec<f64> via 8-byte little-endian chunks, and asserts both byte alignment and exact expected sample count.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `exact_w64_frame_count` — QUIRKY
- spec 3 · read at `ddd9d95e6162` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:03:23Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Reads the W64 file's data chunk size from its header, computes bytes-per-frame from channels, bits_per_sample and encoding (float vs int), divides data chunk length by bytes-per-frame, and asserts/panics if there's a remainder (misaligned data), returning the exact frame count as u64. sample_rate_hz may just be used for a sanity check or debug message rather than the core calculation.
- found: Opens the file and delegates to inspect_exact_w64_pcm with a W64PcmFormatExpectation built from all four format params (sample_rate_hz included as a strict expectation, not incidental), panicking on open failure or validation rejection, and returns the sample_frames field of the result.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `exact_w64_characterization_result` — QUIRKY
- spec 3 · read at `bcc9c91f58ef` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:55:59Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Parses the W64 file at `path` to extract its exact structural characterization (header fields, chunk layout), then validates it against the expected sample_rate_hz, channels, depth, and sample_frames, returning Ok(W64ExactStructure) on success or an Err(String) describing a mismatch.
- found: Maps a depth string ("int24"/"float32"/"float64") to bits-per-sample and a W64SampleEncoding, opens the file, and delegates validation to validate_exact_w64_pcm with a W64PcmExpectation struct, converting any error to a String.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `qualify_w64_exact_integrity_contract` — QUIRKY — TANGLED
- spec 3 · read at `ead2fed99108` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:14:37Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A gated qualification test that runs a DSD-to-W64 (float64 Wave64) conversion through the real pipeline, then verifies the output is byte-exact/lossless by re-decoding and comparing against a known-exact reference (header fields, frame count, sample values) using the various exact_w64_* and probe_ffmpeg_w64_* helpers. It builds and returns a serde_json::Value summarizing the qualification results (e.g. per-check pass/fail and measured values) for release automation to consume, likely panicking/asserting on any mismatch along the way.
- found: For every combination of sample rate, channel count, and bit depth, it sweeps power-of-two amplitude exponents to find the smallest value that survives quantization to nonzero, brackets that threshold with a fine-grained boundary probe to confirm monotonic zero/nonzero transition, runs several control fixtures (all-zero, below/at-boundary, leading/trailing silence impulses) verifying exact structural parsing and independent ffmpeg decode, and assembles a large JSON qualification report with per-cell boundary/structure findings rather than doing a simple lossless round-trip comparison.
- predicted: some · documented: none · derivable: no · legible: some · trap: no
- note: This is a quantization-boundary characterization sweep across a huge parameter grid (10 rates x 2 channel counts x 3 depths x ~350 probe points each), not a simple encode/decode-and-compare integrity check — the name alone undersells how much numerical analysis it does.

### `write_dsf_reference_fixture`
- spec 3 · read at `319ad2cf2259` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:16:33Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Synthesizes a minimal but valid DSF container (header chunk, format chunk with given channels/sample_rate_hz, DSD data chunk) containing some deterministic bitstream (silence or a known analytic pattern), and writes the raw bytes to path, providing a small reference fixture file for the DSD reference-qualification tests to convert/probe against, mirroring the DFF sibling write_dff_reference_fixture.
- found: Delegates DSF container construction to sacd_rs::dsf_writer::DsfWriter (rather than hand-building header/chunk bytes as I guessed), creates it with the given channels/sample_rate, writes a deterministic constant-byte (0x69) interleaved payload sized 32768 bytes per channel, and finishes the writer — matches my prediction's intent (deterministic fixture data) but not the mechanism (uses an existing writer type instead of manual chunk assembly).
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `qualify_default_settings_dsd64_dsf_to_flac` — QUIRKY
- spec 3 · read at `398a2a084ae8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:13:18Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Writes a DSD64 DSF reference fixture (likely a known analytic tone), runs it through the actual conversion pipeline with default settings to produce a FLAC file (invoking real SoX-ng/FFmpeg), then decodes and analyzes the output for integrity/fidelity, returning a JSON Value report of the qualification results rather than a plain pass/fail assertion.
- found: Writes a DSD64 DSF fixture, plans a conversion via plan_conversion with default PipelineSettings, asserts it takes the frozen legacy (non-native-Reference) route, executes the resulting sox/ffmpeg commands directly, publishes via atomic rename, verifies the output format/rate/channels/bit-depth via probe, and returns a JSON summary (status, route, commands run, output sha256) rather than doing DSP fidelity analysis.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: I expected audio-fidelity analysis of the decoded output; instead qualification is structural (probe-based format checks) plus a content hash and command-trail record, not a tone/analyzer-based DSP check.

### `default_settings_dsd64_dsf_to_flac_live_smoke`
- spec 3 · read at `5709b8aa04c4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:41:29Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A thin test wrapper that checks the TONEPOET_REQUIRE_TOOLS gate (skipping/returning early if not set) and then calls the peer qualify_default_settings_dsd64_dsf_to_flac() helper to run the actual live conversion/qualification smoke test using real SoX-ng/FFmpeg tools.
- found: Checks selected() gate flag; if not set, prints a skip message and returns early. Otherwise calls qualify_default_settings_dsd64_dsf_to_flac() and discards its result.
- predicted: full · documented: most · derivable: no · legible: full · trap: no

### `write_dff_reference_fixture`
- spec 3 · read at `18db75262d6e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:21:58Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Writes a minimal valid DSDIFF (.dff) file to `path` containing synthetic DSD bitstream data (e.g. a known test pattern or silence) for the given channel count and sample rate, including the required FRM8/DSD/PROP/FVER chunk structure — analogous to its sibling write_dsf_reference_fixture but for the DFF container format instead of DSF.
- found: Uses sacd_rs::dff_writer::DffWriter to create a DSDIFF file, writes a single deterministic frame of repeated 0x96 bytes sized per channel count, then finishes the writer.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `collect_decoded_dsd`
- spec 3 · read at `64d8cbedffd6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:06:26Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Given a path to a DSD file (DSF/DFF), this decodes it using the crate's decoder and collects the decoded output (likely raw PCM or DSD bitstream samples) into a flat Vec<u8> buffer, presumably for byte-level comparison against a reference decode from an external tool like ffmpeg/sox in the qualification test.
- found: Opens a DSD fixture file via the production sacd_rs::open_dsd_as_decoded_reader, then iterates next_dsd_frame() calls, concatenating each frame's data bytes into a single Vec<u8>.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `assert_not_hard_linked`
- spec 3 · read at `2b43677325ba` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:01:04Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Gets the filesystem metadata (inode number, via MetadataExt on unix) for both `left` and `right` and asserts they differ, panicking with a descriptive message if the two paths turn out to be hard-linked to the same underlying file — used to verify that fixture/output files were actually copied rather than linked.
- found: Compares (dev, ino) of two paths' metadata and asserts they differ, to ensure a "private materialization" of a source file wasn't accidentally hard-linked to the original.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `assert_not_hard_linked` #2 — OBSCURE — TRAP
- spec 3 · read at `7f13f21898bb` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T08:59:50Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Test helper that stats both paths (using std::os::unix::fs::MetadataExt to get dev/ino), and panics/asserts if they share the same device and inode number, confirming the two output files are independent copies rather than hard links of each other.
- found: Just canonicalizes both paths and asserts the resulting absolute paths are not equal — it does not check inode/device numbers at all, so it would not actually detect a real hard link (which by definition has a distinct path from its target but the same inode).
- predicted: none · documented: none · derivable: yes · legible: full · trap: yes
- note: The name promises an inode-level hard-link check but the body only compares canonicalized paths, so it can't catch an actual hard link (different path, same inode) — anyone relying on this to guard against accidental hard-linking in materialization code is not actually protected.

### `key`
- spec 3 · read at `83a6823e6ebf` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:30:12Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A const fn that matches on the AnalyzerPeakPosition enum variants and returns a short static string identifier for each (e.g. "start"/"middle"/"end" or similar), used to name/tag generated test fixtures or output keys.
- found: Matches AnalyzerPeakPosition::Early/Late to the static strings "early"/"late".
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `write_analytic_analyzer_fixture` — OBSCURE
- spec 3 · read at `9bfb08bffba2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T16:11:50Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Invokes the external `sox` binary to synthesize a sine tone at the given sample_rate_hz/channels/normalized_frequency/phase_radians/duration_seconds, scaled so its peak matches true_peak_dbfs, with the actual peak sample positioned per peak_position, writes the WAV to `output`, and returns the actual achieved peak dBFS (which may differ slightly from the requested target due to quantization) for later exact comparison in the test.
- found: A thin wrapper that just forwards all arguments to write_analytic_analyzer_fixture_with_depth with a hardcoded bit depth of 64; it does not itself do any sox invocation or synthesis logic.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: The real synthesis logic lives in write_analytic_analyzer_fixture_with_depth; this is just a default-depth convenience wrapper.

### `write_analytic_analyzer_fixture_with_depth` — QUIRKY
- spec 3 · read at `a0b6aeb66b9d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:23:38Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: This constructs a synthetic analytic test-tone fixture (sine at normalized_frequency/phase_radians, some duration, with peak positioned per peak_position) via SoX, targeting a specific true-peak level and sample rate/channels, then encodes it to `output` at the specified output_bits depth (unlike the sibling write_analytic_analyzer_fixture which likely uses a default depth) — and returns the actual achieved peak level in dBFS after quantization to that bit depth, since the requested peak may not be exactly representable at lower depths.
- found: Synthesizes a windowed sine burst (raised-cosine ramp in/out, early or late positioned active region, per-channel phase offset) as raw f64 samples, writes them to a temp raw file, shells out to SoX to convert to a w64 float file at output_bits (32 or 64) depth, deletes the raw temp file, and returns the analytic peak (20*log10 of the max generated sample magnitude) computed directly from the generated samples rather than by measuring the encoded output.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: Restricted to float depths (32/64) via an assert; my prediction wrongly assumed integer quantization affecting the returned peak.

### `write_analytic_multitone_fixture` — TANGLED
- spec 3 · read at `8e26655269df` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:43:04Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Invokes the sox binary to synthesize a multitone test signal (multiple sine components) at the given sample rate and channel count, scaled/positioned so a specific true peak level occurs at a given sample offset per the requested AnalyzerPeakPosition, writes it to output, and returns the actual measured peak dBFS achieved (which may differ slightly from the requested true_peak_dbfs due to synthesis/rounding).
- found: Manually synthesizes raw f64 PCM samples as a weighted sum of 4 fixed low-frequency cosines with a raised-cosine ramp envelope, positioning the peak within an active window (early or late) offset by peak_offset_samples, writes the raw floats to a temp file, uses sox purely as a format/container converter to w64, deletes the raw temp file, and returns the actual achieved peak level in dBFS.
- predicted: most · documented: none · derivable: yes · legible: some · trap: no

### `key` #2
- spec 3 · read at `3d3141b9fa30` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:14:11Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: AdversarialAnalyzerFixture is likely an enum of adversarial test-signal variants (e.g. edge cases designed to trip up the analyzer), and this const fn is a simple match statement mapping each variant to a short static string identifier, used to name/label generated fixture files, similar to the sibling AnalyzerPeakPosition::key.
- found: Match statement mapping each adversarial fixture variant (Impulse, NearBandEdgeBurst, AlternatingSign, BroadbandDeterministic, BoundaryTransient) to its static string key.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `write_adversarial_analyzer_fixture`
- spec 3 · read at `785c4bf39807` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:12:58Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Test-fixture generator that shells out to sox to synthesize an "adversarial" test signal (a pattern designed to trip up a peak/level analyzer, chosen by the fixture enum), places its peak per peak_position, and writes the result to output at the given sample rate/channel count. Returns the known/expected peak amplitude value that was embedded, for the test to compare against the analyzer's reported measurement.
- found: Generates raw f64 sample data directly in Rust (per-fixture-variant waveform: impulse, near-band-edge burst, alternating sign, broadband deterministic, boundary transient), scales it so the true peak sits at exactly -0.5 dBFS, writes it to a temp raw file, then shells out to sox purely to container-convert that raw f64 data into a w64 file (not to synthesize the signal itself), and returns the constant PEAK_DBFS (-0.5) as the known expected peak.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: sox is used only as a format converter here, not a signal generator — the adversarial waveform math is all hand-rolled in Rust.

### `measurement_with_oversample_factor` — QUIRKY
- spec 3 · read at `bfeed3a1c917` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:54:21Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Clones measurement and returns a modified PlannedMeasurement with its oversample factor (and possibly an internally recomputed effective sample rate) overridden to oversample_factor, used to construct variant true-peak-analyzer test cases at different oversampling levels for the given sample_rate_hz.
- found: Clones the measurement, multiplies sample_rate_hz by oversample_factor, finds the '-s' flag in the underlying SoX command args and overwrites the following arg with the new rate, and appends an oversample-factor note to the command description.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `target_format`
- spec 3 · read at `9d5f8a3f0a93` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:45:05Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A simple match over ResolvedOutputTarget variants (Flac, Mp3, Aac, Opus, WavPack, Dsd, etc.) that returns the corresponding AudioFormat enum value used to configure the pipeline settings for that target — a pure lookup/mapping function with no side effects.
- found: Matches ResolvedOutputTarget variants (FlacNative, WavRiff/WavRf64/WavW64, AiffNative, WavPackNative, AlacM4a) to their AudioFormat equivalent, panicking on any other target as unsupported for this qualification suite.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `target_key`
- spec 3 · read at `cd8084196318` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:53:35Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: This maps a ResolvedOutputTarget enum value to a short static string identifier used as a lookup/label key (e.g., for test fixture naming or result maps), likely via a match over each target variant returning a lowercase slug like "dsf", "dff", "wavpack", etc.
- found: Matches known ResolvedOutputTarget variants to static slug strings, and panics on any other (unsupported) target rather than returning a fallback.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `target_extension`
- spec 3 · read at `5bcc2bcd6512` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:28:53Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Matches on the ResolvedOutputTarget's format/variant and returns a static string like "wav", "flac", or "dsf" naming the file extension appropriate for that output target, used elsewhere in the test to build output file paths.
- found: Matches ResolvedOutputTarget variants (FlacNative, WavRiff, WavRf64, WavW64, AiffNative, WavPackNative, AlacM4a) to their static file extension strings, panicking for any unmapped/unsupported target.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `wavpack_mode`
- spec 3 · read at `ed16e26990b9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:56:31Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A small helper mapping a numeric compression `level` (0-9 or similar) to a WavPackMode variant (e.g., Fast/Normal/High/VeryHigh/Extra), likely via a match on ranges, used to parametrize test fixtures across compression settings for reference qualification.
- found: Maps u8 levels 0-3 to WavPackMode::Fast/Normal/High/VeryHigh; any other value panics with an "invalid WavPack level" message rather than defaulting.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `assert_production_plan_structure` — QUIRKY — TANGLED
- spec 3 · read at `ce59a4b86a74` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:31:44Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: An assertion helper that inspects a ConversionPlan's command chain to verify it matches the expected "production" shape for this pathway (e.g., correct tool ordering, presence/absence of specific flags), and additionally asserts that if expected_compression_level is Some, the plan's compression argument matches it (and if None, no compression flag is present).
- found: Exhaustively asserts the exact shape of a production ConversionPlan: the initial SoX render step's flags and gain/rate ordering, exactly two true-peak measurement steps with specific purposes/parsers/deadlines and target-dependent producer stages, a single deferred terminal SoX command with correct gain-binding count per gain policy, and target-specific packaging (no package for W64, a typed two-process pipeline for Float64 WAV, or a single FFmpeg package step with compression-level checks for FLAC/WavPack).
- predicted: some · documented: none · derivable: yes · legible: some · trap: no
- note: Far more exhaustive than the name suggests — it encodes specific tool-argument-ordering invariants (gain before rate, -ar before -i) that aren't discoverable from the signature or file doc.

### `planned_reference_source_cell`
- spec 3 · read at `a33290053b6b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:30:42Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A test helper that assembles all its many parameters (paths, sample rates, channels, source/target format, reconstruction profile, gain mode, normalization target, compression level) into the pipeline crate's plan_reference_dsd call (or equivalent), producing a ConversionPlan for one cell of a qualification test grid; likely constructs intermediate request/settings structs and derives output paths under `root`.
- found: Builds a PipelineSettings (DSD native_v2, target format/rate/depth, reference policy, profile, gain mode, normalization target, conditional FLAC/WavPack compression) and a PlanRequest with SourceInfo describing the DSD input, creates the work dir, calls plan_reference_dsd, asserts production plan structure via assert_production_plan_structure, and returns the plan.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `planned_reference_cell` — QUIRKY
- spec 3 · read at `185def5afc02` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T10:07:17Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Constructs a ConversionPlan by building a request/config value from the given parameters (root, input path, source/target rates, channels, bit depth, target format, DSD reconstruction profile, gain mode, fixed gain, normalize target dBFS, compression level) and invoking the production planning function (not actually executing the conversion) so tests can assert on the resulting plan's structure.
- found: Thin wrapper delegating to planned_reference_source_cell with AudioFormat::Dsf and DsdSourceKind::DsfUncompressed hardcoded, forwarding all other params through.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The file-level doc describes the whole test file's gating (TONEPOET_REQUIRE_TOOLS), not this specific function.

### `run_planned_command`
- spec 3 · read at `259f7432a0bc` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:08:49Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Given a PlannedCommand enum/struct describing a step in the pipeline, this picks the right binary (sox or ffmpeg) based on which tool the command targets, builds a std::process::Command with the command's arguments, executes it synchronously, and returns the captured Output (stdout/stderr/status). Likely panics or unwraps on spawn failure since this is test code.
- found: It asserts the command's environment policy is ClearAndSet with exactly LC_ALL=C, then selects sox or ffmpeg binary path based on ToolIdentifier (panicking on any other tool), and delegates to a `run` helper with the tool path and args to produce the Output.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `drain_child_stderr`
- spec 3 · read at `810270a381cd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:20:30Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Spawns a background thread that reads the child process's stderr pipe line-by-line, printing each line prefixed with `label` for visibility during long-running test pipelines, while also collecting the output into a shared buffer. Returns an OutputDrain struct (likely holding a JoinHandle plus an Arc<Mutex<...>> or similar) so the caller can join the thread and retrieve captured stderr later without the child blocking on a full stderr pipe.
- found: It's a thin wrapper: takes the child's stderr handle (panicking with the label if stderr wasn't piped) and delegates the actual draining/collecting work to drain_child_output(reader, label), which presumably does the thread-spawn-and-collect work I predicted.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The real logic lives in drain_child_output — this function is just the stderr-specific accessor/guard around it.

### `supervise_qualified_pipeline` — QUIRKY
- spec 3 · read at `8d13108435b9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:58:10Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Waits for both the producer and consumer child processes in a piped test pipeline to exit, drains the producer's stderr via producer_stderr_task, checks both exit statuses and panics/fails with a diagnostic message (including label) on nonzero exit, and returns a PlannedPipelineOutput bundling the collected stdout/stderr/exit info for the test to assert against.
- found: Polls both producer and consumer children with try_wait() until both terminate, a deadline expires, or an inspect error occurs; if either side fails first it force-terminates the other; drains stdout/stderr streams via async drain tasks, accumulates any failure into a single message, panics with full status/output context on failure, and otherwise returns a PlannedPipelineOutput bundling both Outputs.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `run_streamed_measurement_pipeline`
- spec 3 · read at `5da8e40dd5f0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:18:30Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Spawns and supervises the actual OS-level pipeline (sox piped into ffmpeg, per the planned measurement) for the DSD reference qualification harness. It builds Command objects for sox and ffmpeg using the paths given, wires stdout of one to stdin of the other, spawns both processes, drains stderr concurrently to avoid deadlock, waits for both to exit, and packages exit statuses/paths into a PlannedPipelineOutput.
- found: Asserts the planned measurement's producer/consumer stages have the expected typed shape (stdin/stdout wiring, cleared LC_ALL=C environment), resolves tool identifiers to sox/ffmpeg paths, spawns the producer with piped stdout/stderr, drains its stderr concurrently, pipes its stdout into the consumer's stdin, spawns the consumer (cleaning up the producer on spawn failure), delegates actual wait/reap supervision to supervise_qualified_pipeline, then asserts both processes exited successfully before returning the combined output.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `run_planned_command_pipeline` — QUIRKY
- spec 3 · read at `12d63d5c7b31` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:59:25Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Iterates over the steps in `pipeline`, resolving each step's binary to either `sox` or `ffmpeg`, and runs each step in sequence (likely delegating to the sibling `run_planned_command` helper), draining stderr via `drain_child_stderr` as it goes. It collects each step's outcome (status/output/artifact paths) into a `PlannedPipelineOutput`, stopping early and surfacing an error/panic if any step fails.
- found: Asserts the pipeline plan has a sox producer writing to stdout and an ffmpeg consumer reading from stdin, both with a cleared+set 'LC_ALL=C' environment policy, then actually spawns sox with piped stdout, pipes that into ffmpeg's stdin, drains producer stderr concurrently, handles consumer spawn failure by terminating/reaping the producer, and finally supervises both children to completion via supervise_qualified_pipeline, asserting both exit successfully.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `loudnorm_input_tp`
- spec 3 · read at `219a70ddbed3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:48:37Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Parses FFmpeg's loudnorm filter stderr output (which prints a JSON summary block at the end) to extract the "input_tp" (true peak) field, converting the stderr bytes to a string, locating the JSON block, and parsing/returning that value as an f64.
- found: Converts stderr bytes to a string, extracts the single loudnorm JSON report via a helper, parses it with serde_json, and pulls out input_tp (a string field) parsed to f64, panicking with descriptive messages at each failure point.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `require_sparse_file_support`
- spec 3 · read at `a0729a941101` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:54:24Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Creates a temporary file in `directory`, seeks far ahead and writes a small amount of data to create a sparse file, then inspects the actual allocated disk blocks (e.g. via metadata/stat) versus the logical file size. If the allocated size is not significantly smaller than the logical size, it panics with a message explaining the filesystem doesn't support sparse files, since other tests in this suite depend on that support.
- found: Creates a probe file, uses set_len (ftruncate) to size it to 16 MiB rather than seek+write, reads metadata.blocks()*512 as allocated bytes, removes the probe file, then asserts allocated < len/8 with a message naming the specific mandatory >4GiB analyzer-carrier fixture that depends on this.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `require_sparse_file_support` #2 — OBSCURE
- spec 3 · read at `925c1fb8791f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:34:36Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Creates a test file inside _directory, seeks far past the current end and writes a few bytes to create a hole, then compares the file's logical size to its actual on-disk allocated size (e.g. via metadata blocks) to confirm the filesystem supports sparse files, panicking/asserting if it doesn't — used to gate tests needing sparse-file support like create_sparse_w64_capacity_fixture.
- found: Unconditionally panics with a message saying the >4GiB fixture requires Unix sparse-file accounting — this is presumably the non-Unix (#[cfg(not(unix))]) fallback variant (id has #2, implying a differently-cfg'd sibling with the same name does real detection work).
- predicted: none · documented: some · derivable: no · legible: full · trap: no
- note: The '#2' in the id implies a same-named cfg-gated sibling exists elsewhere that presumably does the real sparse-file probing; this variant is just the non-Unix panic path, not derivable from this snippet alone.

### `create_sparse_w64_capacity_fixture`
- spec 3 · read at `2178a152d113` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:40:52Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Copies/adapts a seed Wave64 (w64) file's header to a new output file, patches the chunk-size fields to declare audio_payload_bytes of audio data, then uses filesystem sparse-file seek+truncate (rather than writing real audio data) to make the file appear that large on disk without consuming that much space, for testing capacity/size-boundary handling. Returns the resulting total file size in bytes.
- found: Reads a seed W64 file, locates its fact/data chunk GUIDs, patches the RIFF file-size, fact frame-count, and data chunk-size header fields to reflect audio_payload_bytes, writes just that patched header to the output file, then uses File::set_len to sparsely extend it to the full declared size without writing real audio data. Returns frame_count (not file size as I guessed).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `duration_for_guarded_output_frames` — QUIRKY — TANGLED
- spec 3 · read at `ee662f751a20` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:32:45Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Converts sample_frames and sample_rate_hz into a Duration representing playback time (frames / sample_rate seconds), guarding against a zero sample rate by returning Duration::ZERO instead of panicking/dividing by zero, likely using integer nanosecond math rather than floating point for precision.
- found: Computes a specific capacity-boundary Duration: subtracts a fixed guard-frame constant then one more frame to get the floor of unguarded frames, converts to nanoseconds via checked u128 math rounding up by adding 1ns, builds the Duration, then round-trips by recomputing the frame count that duration would plan (ceil-div) and asserts it matches unguarded_frames — a self-verifying test fixture helper, not a general frames-to-duration converter.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no
- note: Not a general sample-rate-to-duration helper — it's tightly coupled to REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES and a specific boundary-testing invariant, with a built-in self-check assertion.

### `capacity_boundary_plan_result`
- spec 3 · read at `f0563efe6bbb` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:18:16Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Builds a ConversionPlan for the given input path (likely a sparse fixture near a format's size-capacity boundary, e.g. WAV's 4GiB limit) using sample_frames to control the declared duration, and returns the Result from the pipeline's planning function so the caller can assert whether planning succeeds or fails right at the boundary.
- found: Builds a full PlanRequest for a DSD-to-W64/float64 reference conversion (fixed sample rate, SoX-NG reference policy, reconstruction profile, DSF source) with duration derived from sample_frames, creates the work directory, and calls plan_reference_dsd — confirming my guess it plans a boundary case for W64 capacity, but with far more specific pipeline settings (DSD reference decode path, float64 W64 target) than I predicted.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `inspect_streaming_wav_header` — QUIRKY
- spec 3 · read at `bd8417196fed` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:33:13Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Spawns/uses the given PlannedCommand producer (possibly piped through the sox binary at `sox`), reads the initial bytes of its stdout to parse the WAV/RIFF header fields while the rest of the stream may still be generating, and returns a tuple of (sample_rate, channel_count, header_size_in_bytes or bytes_read) so callers can validate the streaming header is well-formed before full data arrives.
- found: Spawns sox with the producer's args under a timeout/watchdog thread, reads exactly 4096 bytes of stdout, parses the RIFF/WAVE chunk structure to locate the "data" chunk, and returns (riff_size_field, data_size_field, data_payload_offset) — not sample rate/channels as I guessed. Heavy machinery for timeout handling and stderr draining around the actual header parse.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `qualify_analyzer_carrier_contract` — QUIRKY — TANGLED
- spec 3 · read at `d01ea7dbe52e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:46:56Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Runs the analyzer against a real DSD carrier signal fixture through actual SoX-ng/FFmpeg tool pipelines (per the file doc), verifying that DSD's ultrasonic noise-shaped carrier doesn't corrupt loudness/peak measurements (or is correctly filtered/detected), and returns a Value (JSON) summarizing the measured results and pass/fail verdict for reporting via --nocapture.
- found: A sprawling multi-part qualification test that: (1) reproduces a pinned FFmpeg direct-decode scaling defect for Float64 W64 vs. the SoX-ng v15 oversampled measurement path, (2) reproduces and bounds a pinned SoX-ng silent-content W64 header defect (false declared extent) and confirms an independent exact parser rejects it with diagnostic DSD-REF-P0-026, (3) validates the Float32 W64 direct FFmpeg-to-SoX measurement path matches analytic peak, (4) runs an isolated F1 reference-gain regression checking pre/post measurements stay gain-consistent under a -1dBTP ceiling, and (5) scans a streamed-WAV capacity boundary (including a RIFF-size field wraparound and full u32 data-size wraparound) verifying planner admission/rejection transitions. Returns a large serde_json::Value bundling all these witnesses/results for reporting.
- predicted: some · documented: some · derivable: no · legible: some · trap: no
- note: The function is really 5+ independent qualification scenarios bundled into one huge test; the name only hints at the DSD carrier aspect and gives no indication of the W64 header defect or streaming capacity wraparound sections.

### `run_planned_measurement` — QUIRKY
- spec 3 · read at `47516ec837ee` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:35:32Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Given a PlannedMeasurement description (likely enumerating a sequence of planned commands/stages to run through sox/ffmpeg), this dispatches to one of the pipeline runner helpers (run_planned_command_pipeline or run_streamed_measurement_pipeline) depending on the measurement's mode, executes the external tool chain using the given sox/ffmpeg paths, and packages the resulting output (e.g. decoded samples, timing/bounds info) into a PlannedMeasurementOutput struct.
- found: Asserts strict contract fields on the measurement (parser type, environment policy, LC_ALL=C env), then branches: if there's an input_stage it runs the streamed pipeline (producer+consumer), otherwise it asserts the command is a bare sox invocation against the carrier path and just runs sox directly, wrapping either result in PlannedMeasurementOutput.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `policy_measurement_bounds` — QUIRKY
- spec 3 · read at `33e8fa1387e5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:08:02Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Returns a pair of tolerance bounds (min, max) in nano-decibel units (DbNano) defining the acceptable measurement window for the DSD reference qualification policy — likely hardcoded constants representing an allowed deviation range around an expected loudness/level target, used elsewhere to assert measured values fall within range.
- found: Loads a qualification JSON fixture (dsd_reference_sox_ng...json) baked in via include_str!, and parses two DbNano fields out of it: analyzer.reporting_uncertainty_db and analyzer.analyzer_residual_db, returning them as a tuple.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Values come from a versioned JSON qualification fixture, not hardcoded constants, and are two distinct named uncertainty sources rather than a min/max bound pair.

### `execute_measurement` — QUIRKY — TANGLED
- spec 3 · read at `f4360eebd0d6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:58:46Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Dispatches on the kind of PlannedMeasurement (render-only vs. terminal chain, matching the peer functions execute_planned_render_only/execute_planned_terminal_chain), invoking sox/ffmpeg subprocesses under root to produce/convert the DSD-derived audio, decodes the resulting samples, and computes a TruePeakMeasurement (true peak level plus maybe channel info) from them for the given channels.
- found: Runs the planned sox/ffmpeg measurement, extracts the true-peak stat from SoX's stderr stats report; if the result is -inf (silence), it independently validates that via a raw f64le scan proving signed-zero samples; then parses the raw value into a TruePeakMeasurement with policy quantization/error bounds, asserting the conservative upper bound equals reported+q+e.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no

### `execute_planned_render_only`
- spec 3 · read at `eeee2965f0d1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:39:35Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Given a ConversionPlan and paths to the sox and ffmpeg binaries, this executes just the "render" stage of the plan (as opposed to the full measurement/terminal chain covered by sibling functions), invoking the external tools as subprocesses, and returns a Vec<String> of the commands/arguments actually run (for qualification logging/inspection), stopping short of any measurement or terminal verification steps.
- found: Takes the plan's reference summary and first planned step (asserted to be a Command whose output matches the reference r64_path), runs it via run_planned_command, asserts the r64 file was created, and returns that single command's args (not a general list of all executed commands).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `execute_planned_terminal_chain` — QUIRKY — TANGLED
- spec 3 · read at `86796b6df506` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:18:18Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Iterates the steps in the ConversionPlan's terminal chain, invoking sox and/or ffmpeg binaries at the given paths under root for each step in sequence, checking exit status/output for failures and returning Err(String) on any failure. When execute_render is false it skips actually running the final render step (dry validation of the plan), otherwise it runs the full chain and returns a PlannedChainResult with the resulting output path(s)/info.
- found: Walks plan.steps() and matches on the step kind: first command must render the R64 reference (optionally skipped if execute_render is false, requiring a pre-existing fixture); a second command or pipeline packages the terminal PCM; Measurement steps run true-peak/gain measurements keyed by MeasurementId, with special validation of the post-final-acceptance measurement against the pre gain-authority measurement; DeferredCommand resolves a terminal command using prior measurements and runs it. After the loop it asserts exactly one deferred/terminal command ran and exactly two measurements were taken with the expected purposes (GainAuthority then PostFinalAcceptance), returning a PlannedChainResult or descriptive Err strings on any violation.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no

### `decode_f64le_samples` — QUIRKY
- spec 3 · read at `588718925fff` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:51:41Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Matches on route (a ReferenceDecodeMechanism enum) to decide how to interpret output's bytes — e.g. skip a WAV/W64 header for one route vs. treat as raw for another — then reads the payload in 8-byte little-endian chunks, converting each to f64 via f64::from_le_bytes, collecting into a Vec<f64>.
- found: Asserts stdout is non-empty and its length is a multiple of 8 (using route only in the panic messages, not for any branching), then converts stdout into a Vec<f64> by reading consecutive 8-byte little-endian chunks. There's no header-skipping or route-dependent parsing at all — stdout is assumed to already be raw f64le samples.
- predicted: some · documented: none · derivable: no · legible: full · trap: no
- note: route is purely cosmetic here (for assertion messages) despite looking like it should drive a decode strategy given the enum name ReferenceDecodeMechanism.

### `direct_ffmpeg_f64_samples`
- spec 3 · read at `db385db4bc8d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:38:34Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Runs ffmpeg directly (as a raw subprocess, not through the pipeline) on `input`, decoding to raw float64 (f64le) PCM output—likely via `-f f64le -` to stdout or a temp file—then parses those bytes into a Vec<f64> (probably delegating byte-parsing to the peer decode_f64le_samples). Used as an independent reference decode path to compare against the pipeline's own decoded output for qualification testing.
- found: Runs ffmpeg directly with explicit flags to select the first audio stream, strip metadata/video/subtitle/data streams, decode via pcm_f64le codec to raw f64le on stdout, then parses via decode_f64le_samples tagged with ReferenceDecodeMechanism::DirectFfmpeg.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `streamed_float64_w64_f64_samples` — QUIRKY
- spec 3 · read at `92140820f1e3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T06:11:50Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Invokes the `sox` binary on `input`, telling it to stream out a W64 (Wave64) container encoded as 64-bit float samples (likely via stdout piping), then parses/decodes that W64 stream into a flat Vec<f64> of sample values for comparison against other decode paths in this reference qualification test.
- found: Runs sox with -S -D flags on the input, outputting raw (not W64-container) 64-bit little-endian floating-point samples to stdout, then decodes that raw stream into Vec<f64> via decode_f64le_samples, tagging it with a ReferenceDecodeMechanism::SoxFloat64W64RawStream route label.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Despite the function name mentioning "w64", the actual sox output format is raw, not the W64/Wave64 container — the name refers to the route/mechanism label, not the container format.

### `decoded_f64_samples`
- spec 3 · read at `6559deecdb3e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:46:05Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Matches on the ReferenceDecodedCarrier variant (raw f64le buffer vs. a file needing sox/ffmpeg decode) and dispatches to one of the sibling helpers (decode_f64le_samples, direct_ffmpeg_f64_samples, streamed_float64_w64_f64_samples) to produce a normalized Vec<f64> of decoded samples, using the sox/ffmpeg paths only when the carrier requires external decoding.
- found: Matches on carrier.authority().mechanism() (a ReferenceDecodeMechanism enum) with exactly two variants, DirectFfmpeg and SoxFloat64W64RawStream, dispatching to direct_ffmpeg_f64_samples or streamed_float64_w64_f64_samples respectively.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `terminal_bound_q63` — QUIRKY
- spec 3 · read at `20064756fa0d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:28:41Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Given a ResolvedGainPolicy, computes a maximum/terminal magnitude bound expressed in Q63-style fixed-point integer representation (related to bit depth or gain scaling), returned as u64, used by assert_terminal_realization_bound to check measured sample values stay within an expected ceiling.
- found: Pattern-matches on the ResolvedGainPolicy enum variant, extracting `terminal_bound.max_added_peak_fs_q63_ceil` for three variants that carry a terminal_bound field, and panics for NormalizePeak since it has no such authority.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: File-level doc describes the overall qualification test's purpose/gating, not this specific accessor helper.

### `assert_terminal_realization_bound` — QUIRKY
- spec 3 · read at `79c25ad13fbb` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:29:46Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Executes the terminal render chain (via sox and/or ffmpeg) using terminal_args against the reference DSD plan, decodes the resulting samples, computes a peak or level measurement, compares it against a bound derived from summary (likely the q63-fixed-point terminal bound), asserts it does not exceed that bound, and returns the measured value as f64.
- found: Extracts the gain dB from terminal_args and converts to a linear multiplier, decodes both an input (R64 reconstruction) and output (terminal QPCM) sample carrier, computes the max absolute error between output and gain-scaled input sample-by-sample, and asserts that error is within a q63 fixed-point bound derived from the plan's gain policy — a numerical-accuracy bound on the terminal gain-applying stage, not a generic peak/level check.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `known_defective_w64_metadata_remux_args`
- spec 3 · read at `78f00a7cc5bc` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T10:01:48Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Returns a Vec<String> of tool command-line arguments (likely ffmpeg) that reproduce a known-defective W64 metadata remux between the given input and output paths, used as a fixture so a qualification test can assert the pipeline correctly detects/rejects this specific known-bad output rather than silently accepting corrupted W64 structure.
- found: Returns a fixed FFmpeg CLI argument list that stream-copies audio into a W64 container while stripping existing metadata and injecting a "title" metadata tag — the specific combination that is known to produce defective W64 structure, used as a negative-control fixture for qualification tests.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `deterministic_int24_mono_bytes`
- spec 3 · read at `0edeb8b5497d` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:03:32Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Generates a deterministic, reproducible buffer of raw 24-bit little-endian mono PCM sample bytes (3 bytes per sample, sample_count samples total) for use as test fixture input, likely computed via a simple deterministic formula (e.g. a fixed seed pseudo-random generator or a sine/ramp pattern based on the sample index) so tests get identical bytes across runs.
- found: Builds sample_count deterministic 24-bit little-endian mono PCM samples using a simple multiplicative hash of the index (index*7919+1337 masked to 24 bits, then offset to signed range) and appends the low 3 bytes of each to the output buffer.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `sox_raw_int24_mono_container`
- spec 3 · read at `381f87546544` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T10:24:24Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Builds and runs a SoX command-line invocation that takes a raw signed 24-bit mono PCM input file and repackages it into a container of the given output_type (e.g. "wav" or "w64"), writing to output. It likely constructs args specifying raw format parameters (bit depth, channels, sample rate, encoding) for the input and the target format for the output, then executes the process and asserts/panics on failure.
- found: Runs sox with explicit raw-format flags (signed-integer, 24-bit, little-endian, 88200 Hz, mono) to read the raw input, then writes it out as int24 signed-integer in the given container type/output_type, using the shared `run` helper.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `ffmpeg_decode_int24_bytes`
- spec 3 · read at `f472c927389d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:08:21Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Runs the given ffmpeg binary against `input`, invoking it with args to decode/remux to raw signed 24-bit little-endian PCM piped to stdout (e.g. `-f s24le -`), captures that stdout, and returns it as a Vec<u8> of raw interleaved int24 sample bytes — used as a decode-authority reference for comparing against the pipeline's own DSD-to-PCM output.
- found: Invokes ffmpeg via a `run` helper with explicit flags (-hide_banner, -nostdin, -map 0:a:0, -f s24le, -c:a pcm_s24le, pipe:1) decoding the first audio stream of `input` to raw s24le PCM on stdout, returning just the captured stdout bytes.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Uses an explicit stream map (0:a:0) and explicit codec (pcm_s24le) alongside the format flag — a detail not guessable from the signature alone.

### `qualify_alignment_metadata_mutation_probes`
- spec 3 · read at `4713a31bbeb8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T05:31:52Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Probes whether writing track/album metadata into an audio container (likely W64/WAV, given the peer known_defective_w64_metadata_remux_args) can silently shift or corrupt sample alignment. It decodes samples before and after applying the metadata_runner mutation via sox/ffmpeg, compares them for exact match, including at least one known-defective remux path as a regression check, and packages the pass/fail evidence into a JSON Value for the qualification report.
- found: Runs two alignment probes with deliberately non-block-aligned int24 mono PCM sizes: (1) a W64 container remuxed via a known-defective ffmpeg metadata path, asserting the defect (a phantom trailing zero sample) is reproduced and documented as a rejected route with a specific rejection code; (2) a RIFF/WAV file run through the real production qualify_production_metadata_mutation path, asserting sample data is byte-identical before and after and that ffmpeg was the primary mutator. Returns a JSON summary of both probes' outcomes.
- predicted: most · documented: none · derivable: no · legible: most · trap: no
- note: The W64 probe is intentionally documenting a known muxer defect (phantom trailing sample) as rejected/unsupported rather than testing for correctness — worth knowing before assuming all assertions here mean "this path works."

### `record_decode_authority`
- spec 3 · read at `62d74bc921ba` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:41:00Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Increments counters in route_counts and encoding_counts keyed by some combination of `phase` and fields from `authority` (e.g. which decode route/tool and which encoding were used), so the test can later assert the expected distribution of decode routes/encodings across a qualification run.
- found: Asserts authority.hash_format() equals the expected constant, then increments route_counts and encoding_counts keyed by "{phase}:{mechanism/encoding key}" for later distribution assertions.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Missed the assert_eq! sanity check on hash_format before tallying.

### `qualify_lossless_package_cells` — QUIRKY — TANGLED
- spec 3 · read at `0563e405ff4a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:11:23Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Iterates over a matrix ("cells") of lossless output package configurations (e.g. combinations of container format, sample rate, bit depth) for the DSD-to-PCM pathway, actually invoking the real SoX-ng/FFmpeg tools for each cell to convert a DSD source and verify the output package's correctness/bit-exactness, while cross-checking against forbidden_route_regression (a JSON blob of previously-known-bad routes) to ensure none of those forbidden conversion routes are silently taken. It accumulates per-cell pass/fail evidence into a PackageQualificationEvidence struct that it returns, likely also asserting/panicking on any cell that fails qualification.
- found: Iterates a full grid of sample rate × channels × bit depth × output target × compression level (480 cases) for the DSD→PCM pathway, for each cell running the actual planned terminal chain through real SoX/FFmpeg/metaflac/wvtag/AtomicParsley tools, asserting exact package probes, decode-authority routes, sample-hash identity before/after packaging and metadata mutation, W64's special metadata-mutation rejection path, and dozens of exact aggregate counts, then returns a PackageQualificationEvidence with a large embedded JSON oracle summary including the forbidden_route_regression argument.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no
- note: This is an exhaustive, tool-gated 480-case qualification harness with many hardcoded exact-count assertions (e.g. case_count==480, specific per-mutator counts) — any change to the grid or routing logic requires updating all these magic numbers in lockstep, which is easy to get subtly wrong.

### `gain_arg`
- spec 3 · read at `0cf5451dab57` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T09:51:33Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Small helper that scans a command's argv for a gain-related flag (likely -v/--gain/SoX's vol/-g) and returns the following argument value as Option<&str>, used by qualification tests to inspect what gain value was passed to the underlying tool.
- found: Finds the first adjacent pair of args where the first equals the literal string "gain" (SoX's gain effect name, not a dash-flag) and returns the following element as the gain value.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `gain_policy_evidence`
- spec 3 · read at `9b661b154b74` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T08:12:30Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Builds a serde_json::Value evidence record for the qualification report: includes fields from the ResolvedGainPolicy (e.g. target/applied gain value, mode) and the terminal_args slice, and cross-checks whether terminal_args actually contains the gain flag/value implied by the policy (likely via the gain_arg helper), so the report can assert the production gain chain matches the resolved policy.
- found: Matches on the ResolvedGainPolicy variant and builds a serde_json object per-variant recording the requested/applied gain, ceilings, terminal bound fields (digest, q63 ceiling, safe pre-terminal ceiling), and reserve constant; NormalizePeak is a simpler case with just target_dbfs and applied gain.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: Docs at file level only describe the gating env var, not this function's per-variant evidence shape.

### `qualify_true_peak_analyzer_authority` — TANGLED
- spec 3 · read at `698b75181b76` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:49:21Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A large qualification test function that generates or loads reference audio fixtures, runs the project's true-peak analyzer against them, and cross-checks results against an external authority (SoX-ng/FFmpeg based measurement) across several test cases (e.g. inter-sample peaks, clipping, full-scale signals). It assembles evidence (measured vs expected values, pass/fail per case) into a JSON `Value` used as part of the release qualification report/gate.
- found: Runs a huge combinatorial sweep (rates x channels x frequencies x phases x levels x durations x peak-positions) of synthetic single-tone, fixed-frequency, multitone, and adversarial fixtures through the planner's true-peak measurement pipeline, asserting under/over-report bounds, monotonicity, and conservative-bound correctness against a 64x pinned-tool oracle for adversarial cases, then returns a JSON evidence report with per-cell stats and a SHA-256 digest of all case data.
- predicted: most · documented: none · derivable: yes · legible: some · trap: no

### `qualify_analyzer_deadline_model` — TANGLED
- spec 3 · read at `d31364027098` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:39:00Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Runs the true-peak/loudness analyzer over one or more DSD/PCM reference files while measuring wall-clock time, then checks the measured durations against an expected deadline/performance model (e.g. a linear or bounded relationship to file size or sample count) to ensure the analyzer stays within acceptable processing-time bounds. Returns a serde_json::Value summarizing the timing evidence (input sizes, measured durations, pass/fail against the deadline) for the release qualification report.
- found: Plans and executes a true-peak measurement on a synthetic DSD-derived reference file, asserts the planned expected_duration matches a formula-derived deadline, asserts actual elapsed time stays under that deadline, then asserts observed sample-throughput meets a policy floor and that a max-workload-derived deadline constant matches a pinned constant. Returns a JSON qualification report with all these figures.
- predicted: most · documented: none · derivable: yes · legible: some · trap: no
- note: The deadline model has two layers: a per-run deadline (elapsed < expected_deadline) and a separate throughput-floor/max-workload sanity check on the deadline formula's constants — the name only hints at the first.

### `qualify_production_measurement_gain_terminal_chain` — TANGLED
- spec 3 · read at `ac5386e40aa5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:54:49Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This is a qualification test in a DSD reference test suite that verifies gain is correctly applied at the terminal end of a production measurement chain. It likely renders/encodes audio with a specific gain value using the pinned SoX/FFmpeg toolchain, decodes it back, measures RMS/true-peak via the oracle tools, and asserts the measured gain matches the planned/expected gain within tolerance. It probably builds up a JSON Value report summarizing the pass/fail evidence for release qualification, iterating over several gain values or test cases.
- found: Builds a JSON report by running several gain-mode qualification cases (reference/native/fixed/normalize/silence) through a planned terminal chain, asserting exact gain args, true-peak bounds, dither behavior, unsafe-gain refusal error codes, and a strict true-peak parser probe with quantization/error bounds, inserting per-case evidence into a results map.
- predicted: most · documented: none · derivable: yes · legible: some · trap: no

### `qualify_production_source_front_end_integration` — QUIRKY — TANGLED
- spec 3 · read at `a736d7aba5cb` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:40:45Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A gated qualification test that exercises tonepoet's actual production DSD source/front-end code path (not a synthetic harness) end-to-end against a real or reference file, using the pinned SoX-ng/FFmpeg tools to independently decode/measure the output, then compares results (e.g. sample values, gain, RMS) for agreement and returns a JSON Value summarizing pass/fail evidence to be folded into the overall qualification report.
- found: Exercises the real qualify_reference_source_materialization production seam across a matrix of DSF/DSDIFF uncompressed formats x sample rates x channel counts, verifying byte-identical materialization, planner front-end selection, and rendered W64 output via sox/ffprobe; separately builds a DSDIFF/DST fixture, verifies decode matches an independent oracle, checks tamper detection of the identity digest, and asserts CMPR-classification-mismatch and corrupted-DSTC inputs are rejected with specific error messages; returns a serde_json report aggregating all these results.
- predicted: some · documented: some · derivable: no · legible: some · trap: no

### `qualify_dst_oracle_fixture_authority` — QUIRKY — TANGLED
- spec 3 · read at `8244bc592560` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:40:33Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Iterates over the DST test fixture corpus, decoding each fixture through the pinned reference toolchain (SoX-ng/FFmpeg) and comparing the result byte-for-byte (or via RMS/amplitude checks) against the fixture's expected/stored reference output to confirm the fixtures are trustworthy "oracles". It records each fixture's pass/fail via record_decode_authority and tallies totals into a DstQualificationCounts struct that it returns, which the companion test p0_dst_oracle_fixture_authority_is_complete_and_byte_exact asserts against (e.g. all fixtures checked, zero mismatches).
- found: Verifies a hardcoded 12-case DST fixture corpus embedded via include_bytes!: checks each fixture's SHA256 against a checksums manifest, validates a JSON provenance document's fields (schema version, oracle identity, generator/oracle script hashes, attestation document hash), decodes each DST frame in-process via sacd_rs::dst::decode_frame_with_rate and asserts byte-exact match against expected PCM/DSD output, then round-trips it through raw encode/decode. It also checks rate/channel coverage invariants, computes a canonical corpus digest and compares against published constants, and separately exercises plan_reference_dsd to confirm it rejects unsupported channel counts/sample rates for predictive compressed DST and accepts only DSD64 stereo, finally asserting the tallied DstQualificationCounts match fixed expected values.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no
- note: No SoX/FFmpeg subprocess invocation here despite the module doc mentioning reference toolchains — this function is pure in-process checksum/provenance/roundtrip verification plus pipeline planning-rejection assertions.

### `p0_dst_oracle_fixture_authority_is_complete_and_byte_exact` — QUIRKY
- spec 3 · read at `1be76a66b8cc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:03:48Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A short qualification test that calls the peer helper `qualify_dst_oracle_fixture_authority()` and asserts on its returned evidence/report, checking that the DST oracle fixture (the reference decoder used to validate DST-compressed DSD decoding) is fully accounted for and its output matches expected bytes exactly, likely recording the result into a shared qualification report via record_decode_authority.
- found: Calls qualify_dst_oracle_fixture_authority() and asserts the returned DstQualificationCounts exactly matches a pinned breakdown (12 total fixtures split across predictive_independent_oracle: 6, predictive_stereo_reference: 3, predictive_six_channel_decoder_only: 3, standards_literal_geometry: 6) — a fixed inventory check, not a byte-content comparison.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `sox_rms_amplitude` — OBSCURE
- spec 3 · read at `b00f9a8694e2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:29:39Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Runs the sox binary on input with `-n stat` (or `stats`), captures its stderr output, finds the line containing "RMS amplitude", extracts the trailing numeric value, and parses/unwraps it into an f64 — panicking on any failure since this is test qualification code.
- found: Thin wrapper delegating to rms_amplitude_in_window(sox, input, "0.5", "3.0") — a fixed time window — rather than doing any sox invocation or parsing itself.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: The actual sox invocation/parsing logic lives in the peer rms_amplitude_in_window; this function is just a fixed-window convenience call.

### `rms_amplitude_in_window`
- spec 3 · read at `c4fff68b3201` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:43:05Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Shells out to the `sox` binary on `input`, applying a `trim start duration` effect to select the window and a `stat` (or similar) effect to compute RMS amplitude, capturing stderr/stdout text output. It then parses the "RMS amplitude" line out of sox's textual stats output with string splitting and parses it into an f64, panicking/unwrapping if the expected line isn't found.
- found: Runs sox with `-n trim start duration stat` (null output, just stats), combines stdout/stderr, and parses the "RMS amplitude:" line into an f64, panicking with the full text if the line is absent.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `planned_render_command` — QUIRKY
- spec 3 · read at `a2f036058423` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:39:56Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Builds a conversion plan for rendering `input` from `source_rate_hz` DSD to `target_rate_hz` PCM using the given reconstruction `selection` and optional `fixture_profile`, then extracts the concrete external command (SoX or FFmpeg) that the pipeline would run along with the output file path it would produce under `root`. Used by the qualification tests to inspect/assert on the exact command-line arguments before actually running it.
- found: Has two branches: if a fixture_profile is given, it builds a synthetic/fixture render transcript directly (bypassing real planning) targeting a fixed output path. Otherwise it calls planned_reference_cell to build a real Reference-pathway plan, extracts the first execution step (asserting it's a Command, panicking otherwise) and pulls the r64 output path from the plan's reference summary, returning both.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `planned_response_db`
- spec 3 · read at `fb9af4c780b5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:53:12Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Builds a "planned" render command for a given frequency/rate combination using planned_render_command, executes it through the sox/ffmpeg pipeline to produce an output file under root, then measures the resulting signal's RMS amplitude (via rms_amplitude_in_window or sox_rms_amplitude) and converts it to a dB value representing the frequency response at that point — used to build up a response curve for qualification testing.
- found: Synthesizes a mono sine-wave input tone at the target frequency via sox, renders it through the planned pipeline, measures RMS amplitude before/after via sox_rms_amplitude, computes response in dB with a known -12dB headroom correction added back, then cleans up the fixture files.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: The -12dB headroom compensation is explained only by an inline comment; without it the returned dB values would look mysteriously offset from a naive filter-response calculation.

### `assert_planned_w64_bridge` — QUIRKY
- spec 3 · read at `3533eadfbb08` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:50:17Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Builds/obtains a planned render command that bridges DSD through a W64 intermediate at target_rate_hz, runs it using the given sox/ffmpeg tool paths under the test root, then decodes the resulting output (likely via ffmpeg_decode_int24_bytes and/or sox_rms_amplitude) and asserts the measured amplitude/content matches expectations within tolerance, panicking with a descriptive message if the qualification check fails.
- found: Synthesizes a DSD-rate (2,822,400 Hz) two-tone W64 input via sox synth, builds a Reference-mode planned_reference_cell targeting target_rate_hz/Float64/WavW64, extracts the first planned step as a Command, runs it, then verifies via sox --i queries that the output's sample rate, channel count, and duration (within one-sample tolerance) match expectations before cleaning up the fixture files.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: I expected audio-content/amplitude verification (ffmpeg decode + RMS) but it only checks structural metadata (rate/channels/duration) via sox --i, not sample content.

### `qualify_pinned_reference_toolchain_and_profile_responses` — QUIRKY — TANGLED
- spec 3 · read at `8a69395af83c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:50:48Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Verifies the pinned SoX-ng and FFmpeg toolchain binaries (from the flake-owned paths) are the expected/qualified versions, then runs them against a battery of known reference test signals to capture and profile their decode/measurement responses (e.g. gain, RMS, peak values). Assembles the toolchain identity info and profiled response data into a serde_json::Value report section, to be composed into the larger P0 reference qualification report alongside sibling qualify_* sections.
- found: Verifies exact pinned versions/probes of five tools (SoX-ng, FFmpeg/ffprobe, metaflac, wvtag, AtomicParsley), cross-checks a qualification manifest JSON's status/digests against a preserved candidate snapshot and in-process build/fixture identities, measures actual DSD-to-PCM frequency response (interior/nominal-bandwidth/stopband dB) across several named rate/profile combinations against hard-coded tolerance bounds, verifies each tool's activation path canonicalizes to its declared Nix store path, and assembles all of this (paths, SHA-256 hashes, versions, platform, and measured response profiles) into one large JSON report Value.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no

### `write_report_atomically`
- spec 3 · read at `87dca7b21414` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:06:06Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Writes a JSON Value report to disk atomically — serializes to a temp file in the same directory as the target path, then renames it over the destination path, to avoid partial/corrupted report files if the qualification test process is interrupted mid-write.
- found: Acquires an exclusive file lock (panicking if another writer holds it) to serialize concurrent report writers, then writes JSON to a unique same-directory temp file, syncs it, persists/renames it atomically over the destination, syncs the parent directory for durability, and releases the lock.
- predicted: most · documented: none · derivable: no · legible: most · trap: no
- note: The inline comment explains the lock's purpose (serialize commissioned gates) but a doc-comment on the function itself would help since the locking/fsync-parent durability logic isn't visible from the signature at all.

### `complete_p0_reference_qualification_report` — QUIRKY — TANGLED
- spec 3 · read at `3f562e3be509` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:48:01Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: This is the top-level orchestrating test: it checks whether TONEPOET_REQUIRE_TOOLS is set and returns early (inert) if not, then calls the various qualify_* helper functions (analyzer authority, gain policy, DST oracle fixture, pinned toolchain, lossless package cells, etc.), collects their results into a single qualification report structure, and writes it atomically to disk via write_report_atomically, asserting overall success/completeness.
- found: Gated (returns early unless env selected) top-level test that calls ~12 qualify_* helper functions, loads a pinned qualification manifest JSON fixture, and assembles a very large, highly detailed JSON report (toolchain, DST oracle counts, terminal error bounds, cell contract stats, provenance hashes, etc.) which it writes atomically to a report path and logs.
- predicted: some · documented: most · derivable: no · legible: some · trap: no

## tests/dsd_reference_settings_sentinel.rs

### the file itself
- spec 3 · read at `a5f9f93f3849` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:15:20Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A Rust test suite acting as a "sentinel" inventory guard for the native-v2 DSD settings schema: it enumerates/locks the additive DSD-related settings fields introduced in native-v2 manifests (separate from a frozen legacy-v1 fingerprint/sentinel elsewhere), so that adding/removing a field forces a deliberate update here. Tests likely cover: round-tripping native settings through serialization, verifying the inventory list matches the actual serialized wire fields with no duplicates, confirming migration from legacy v1 to native v2 is idempotent and preserves existing controls, that pre-promotion defaults exactly match legacy v1 wire output, that legacy PCM-to-DSD edits remain flat and survive migration, and that immutable identity/reference fields are persisted and hashed independently from the rest of the fingerprint.
- found: A Rust test file guarding the native-v2 DSD settings schema: it round-trips serialization, checks the field-path inventory constant matches the actual serialized wire with no duplicates, verifies legacy-v1 default output is byte-exact and legacy PCM-to-DSD edits survive migration to native-v2 unchanged, checks migration is idempotent, and (a detail I did not predict) individually mutates every native-v2 field to confirm each one changes the v2 settings fingerprint and none collide, plus pins the exact wire token string for one identity/policy-version field so an append-only enum variant can't silently vanish from the hash.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The per-field fingerprint-uniqueness sweep (mutate one field at a time, assert fingerprint changes and doesn't collide with any other mutation) and the exact-string pinning for DsdReferencePolicyVersion are the file's real substance and aren't hinted at in the header docs.

### `native_settings`
- spec 3 · read at `578c97fd421c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:48:47Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Builds and returns a PipelineSettings value with default() as a base plus specific native-v2 DSD fields overridden to distinctive non-default values, serving as a shared fixture for the round-trip and fingerprint/hash sentinel tests in this file.
- found: Builds default PipelineSettings and swaps only the dsd field for DsdSettings::native_v2(), a single named constructor rather than manually setting distinct fields — simpler than predicted but same overall fixture role.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `round_trip`
- spec 3 · read at `b4c862abe755` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:22:40Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Helper used by other sentinel tests: serializes the given PipelineSettings to its persisted wire format (e.g. JSON) and deserializes it back into a PipelineSettings, returning the result so callers can assert the round trip preserves the original value/fields.
- found: Serializes PipelineSettings to JSON bytes via serde_json::to_vec, then deserializes it back, returning the round-tripped value; panics with expect() on either failure.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `serialized_native_dsd_paths`
- spec 3 · read at `26252e57ef7f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:09:15Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Serializes settings to a JSON value, then recursively walks the DSD-related subtree collecting dotted key paths as strings into a BTreeSet<String>, used by sentinel tests to detect additions/removals in the native-v2 DSD field inventory.
- found: Serializes settings to JSON, always inserts a fixed "dsd.schema" path, then recursively walks only the dsd.pcm_to_dsd and dsd.from_dsd objects (panicking if either isn't an object), inserting a dotted path for every leaf field into a BTreeSet<String>.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `pre_promotion_default_is_exact_legacy_v1_wire`
- spec 3 · read at `ae58b72aafec` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:49:11Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Constructs the pre-promotion default settings value, serializes it to JSON, and asserts it exactly equals a hardcoded legacy-v1 wire JSON string/snapshot, ensuring no new fields have crept into the default that would break old manifests.
- found: Serializes default PipelineSettings, asserts the dsd object is not native-v2, lacks schema_version/from_dsd keys, and has specific legacy default values (lowpass=Auto, gain_mode=Disabled), then round-trips it back through deserialize and checks equality and non-native-v2 status.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `legacy_pcm_to_dsd_edits_remain_flat_and_survive_native_migration`
- spec 3 · read at `ce7c7326e8be` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:44:03Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Builds a legacy-v1 settings value that has PCM-to-DSD conversion edits set (flat, non-nested fields), then runs it through the native-v2 migration path. Asserts that after migration those same edit fields are still present and equal to their original flat values, i.e. migration doesn't drop, nest, or rename them.
- found: Sets a specific edited field (noise_shaper=Crfb) on default legacy PipelineSettings, serializes to JSON and asserts the legacy wire shape is flat (dsd.noise_shaper, no schema_version key). Then migrates to native_v2 and asserts is_native_v2() is true and the edited field value survived the migration.
- predicted: most · documented: some · derivable: no · legible: full · trap: no

### `native_v2_migration_is_idempotent_and_preserves_existing_controls`
- spec 3 · read at `3dc0b239c98d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:18:33Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Constructs settings with some existing user-set controls (e.g. legacy v1 fields), runs the native-v2 migration once and then again on the result, and asserts both that the second run produces an identical output to the first (idempotence) and that the originally-set control values were preserved rather than reset to defaults.
- found: Sets several from_dsd control fields on already-native settings, calls migrate_to_native_v2() once, and asserts the migrated result equals the original (single-call idempotence against already-migrated data), rather than explicitly comparing two successive migration calls.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `native_v2_dsd_inventory_matches_serialized_wire_and_has_no_duplicates`
- spec 3 · read at `a0ab7826f2e4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:12:04Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: This test compares a manually maintained list of native-v2 DSD field paths (the "inventory") against the set of paths produced by actually serializing a native settings instance to its wire format via serialized_native_dsd_paths, asserting the two sets match exactly and that the inventory list itself contains no duplicate entries — guarding against fields being added to the wire format but forgotten in the fingerprint inventory.
- found: Collects SETTINGS_SNAPSHOT_V2_DSD_FIELD_PATHS into a BTreeSet, asserts its size equals the separately declared SETTINGS_SNAPSHOT_V2_DSD_FIELD_COUNT constant (catching duplicates), then asserts it equals the set of paths serialized_native_dsd_paths actually produces from a native_settings() instance.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `native_v2_reference_fields_are_persisted_and_fingerprinted_independently`
- spec 3 · read at `f018a92035b4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:05:57Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Iterates over each field in the native-v2 DSD reference settings inventory, mutates one field at a time from a base settings value, and asserts both that the change round-trips through serialization (persisted) and that it changes the settings fingerprint/hash, while verifying that mutating one field doesn't affect the fingerprint contribution expected from another (independence check), likely using a list of (field name, mutator) pairs.
- found: Builds a list of settings variants each mutating exactly one native-v2 DSD field from baseline, round-trips each through serde, and asserts each variant's fingerprint both differs from baseline and is unique among all variants (a BTreeSet catches any two fields colliding to the same fingerprint).
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: \"Independently\" in the test name means each field produces a distinct, non-colliding fingerprint versus every other field's mutation (via a BTreeSet uniqueness check), not that mutating one field leaves another's contribution unaffected.

### `native_v2_immutable_identity_fields_are_serialized_and_hashed` — QUIRKY
- spec 3 · read at `99c035fb01f0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:04:09Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Builds native-v2 DSD settings via the native_settings helper, sets/verifies specific immutable identity fields, checks they appear in the serialized wire output (serialized_native_dsd_paths) and that they contribute to the settings fingerprint/hash, asserting the hash changes when those identity fields change (to guard against silently losing identity data across serialization).
- found: Builds native_settings(), checks serialized JSON has schema_version 2 and the expected reference_policy value, asserts the v2 snapshot fingerprint is unchanged after a serde round-trip, and pins the exact wire string token for a specific DsdReferencePolicyVersion enum variant so it can't silently change.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

## tests/fixtures/dvda_lpcm_foo_reference_vectors.cpp

### the file itself
- spec 3 · read at `d4c864a572a4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:15:55Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A standalone C++ program (not linked into the main build) that reimplements foo_input_dvda's DVD-Audio LPCM group-decoding algorithm to generate deterministic reference test vectors. It uses a small LCG PRNG (Lcg) to synthesize raw LPCM payload bytes (make_payload/append_random_bytes), decodes them per the reference channel-grouping/bit-packing scheme (decode_group, decode_reference, raw_group_size, source_order, wave_indices), and main() prints the resulting decoded samples/payloads as hex strings (hex, append_i32le/append_frame) so Rust tests can embed them as expected-output fixtures and diff Tone Poet's own LPCM unpacker against this reference.
- found: Generates DVD-Audio LPCM reference vectors: iterates 21 channel-assignment layouts x bit depths x group2/group1 rate ratios, synthesizes deterministic pseudo-random raw payloads via an LCG, decodes them with a reference implementation of foo_input_dvda's group-packing/channel-assignment algorithm, and prints tab-delimited hex lines (params + payload + source-order + wave-order decoded PCM) to stdout for Rust tests to parse as fixtures.
- predicted: most · documented: most · derivable: no · legible: not judged · trap: no
- note: Header doesn't mention the actual output format (tab-delimited hex-encoded lines to stdout) or that group2_bits > group1_bits combinations are deliberately skipped as invalid DVD-A packing.

### `Lcg`
- spec 3 · read at `2f81b3b95a5c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:52:06Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Constructor for a simple linear congruential generator struct; just initializes the `state` member from the given seed via the member-initializer list, with an empty body.
- found: Exactly as predicted: explicit Lcg(uint32_t seed) : state(seed) {}.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `next_u8`
- spec 3 · read at `bb820aa5ae69` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:57:18Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A method on the Lcg (linear congruential generator) struct that advances the internal PRNG state using the LCG recurrence and returns one byte derived from the new state (e.g. a shifted/masked portion of it), used to generate deterministic pseudo-random test payloads.
- found: Standard Numerical Recipes LCG (state = state*1664525 + 1013904223), returns the top byte (bits 24-31) of the new state.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no
- note: The file_doc explains the fixture's purpose well; this specific LCG constant choice (Numerical Recipes) is a nice-to-know but not essential.

### `raw_group_size` — QUIRKY
- spec 3 · read at `370ad8de5a70` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:02:26Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Computes the byte size of one DVD-Audio LPCM "group" — samples are packed in pairs of channels with 16-bit words plus extra low-order bytes appended for 20/24-bit depths — so it likely computes ceil(channels/2) pairs times (2 bytes per channel per 16-bit core, plus 1 or 2 extra bytes per pair depending on whether bits==20 or 24), returning the total raw byte count for that group across all channels.
- found: Just a simple formula: channels * bits / 4 (integer division), not the pairwise-channel-with-extension-bytes logic I predicted based on real DVD-Audio LPCM packing rules.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The real DVD-Audio group-packing scheme (pairs of channels, 16-bit core plus extension bytes for 20/24-bit) is not reflected here — this fixture uses a reduced formula (channels*bits/4), so don't assume it mirrors the actual on-disc bit layout.

### `append_random_bytes`
- spec 3 · read at `3ea73039be12` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:24:11Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Loops count times, calling rng.next_u8() each iteration and pushing the resulting byte onto out — a simple helper to fill a vector with deterministic pseudo-random bytes using the LCG.
- found: Loops count times pushing rng.next_u8() onto out.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `make_payload` — QUIRKY
- spec 3 · read at `c66b7f86cbb6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:40:03Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Builds a synthetic raw LPCM byte payload for testing: seeds an Lcg RNG from `seed`, computes per-group sample byte sizes from group1_bits/group2_bits via raw_group_size, and loops appending pseudo-random bytes via append_random_bytes/append_frame for some number of frames (determined by `ratio`, group2-per-group1 sample count), returning the assembled vector<uint8_t> representing packed LPCM samples for the given channel `assignment`.
- found: Looks up the channel layout for `assignment`, computes byte sizes for group1/group2 via raw_group_size, then loops a fixed number of `steps` (4 if no group2, else ratio+1) appending random bytes: group2 bytes are appended once every `ratio` group1 appends (interleaved via a counter), group1 bytes appended every step. I predicted RNG seeding and group-size computation correctly but missed the specific interleaving cadence and wrongly expected append_frame to be used (it wasn't called at all).
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `decode_group` — TANGLED
- spec 3 · read at `a6ede6282d40` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:13:27Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Decodes one "group" of packed DVD-Audio LPCM samples from a raw block for the given channel count and bit depth. DVD-Audio LPCM packs samples deeper than 16 bits by storing the top 16 bits per sample first and the extra low bits (4 or 8) packed separately afterward, so this reads the 16-bit words then appends the extra bits (indexed differently depending on whether this is group1 or group2, hence the `group2` flag) to reconstruct full-precision 32-bit integer samples, returning them as a vector.
- found: Decodes 2*channels samples per group: for 16-bit reads the two-byte word directly shifted into the top of an i32; for 20-bit it pulls a shared nibble byte per sample-pair, selecting high or low nibble based on parity and the group2 flag; for 24-bit it reads a full extra byte per sample; then reassembles into a left-justified 32-bit sample.
- predicted: most · documented: none · derivable: yes · legible: some · trap: no
- note: The file_doc explains the fixture's purpose but not this function's bit-packing logic (nibble selection for 20-bit, byte layout for 24-bit) — that had to be reverse-engineered from the shifts/masks alone.

### `source_order` — QUIRKY
- spec 3 · read at `58572428220a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:31:43Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Given a channel Assignment (channel-count/layout), returns a vector of channel-name strings (like "L","R","C","LFE","Ls","Rs") in the order foo_input_dvda's decoding model expects samples to appear in the raw LPCM stream, likely via a switch/if-chain over the assignment enum value.
- found: Simply concatenates layout.group1 and layout.group2 (already-named channel-string vectors on Assignment) into one ordered vector, rather than deriving names from an enum/switch.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `wave_indices`
- spec 3 · read at `aa8b98283aed` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:12:20Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Given a channel Assignment/layout, returns a vector of indices describing the WAVE-file channel order (e.g. mapping DVD-Audio's per-layout channel assignment table to standard WAV channel positions) used when generating or comparing reference LPCM vectors.
- found: Gets the source channel name order from source_order(layout), builds a target order by picking out known channel names in a fixed canonical priority (L, R, C, LFE, Ls, Rs, S) followed by any leftover channels in original order, then returns the indices into the source vector that would produce that target ordering.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `append_i32le`
- spec 3 · read at `a2e4df78204f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:57:57Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Appends the four bytes of the int32 value to the out vector in little-endian order, extracting each byte via right-shift and mask (or static_cast) in a loop or four explicit push_back calls.
- found: Converts value to uint32 and push_backs its four bytes in little-endian order via shift/mask, exactly as predicted.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `append_frame`
- spec 3 · read at `60aea796d173` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:29:32Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Iterates over `order` (a channel reordering/index list), and for each index appends the corresponding sample from `frame` to `out` as little-endian bytes via append_i32le, effectively writing one frame of interleaved PCM samples in the given channel order into the output byte buffer.
- found: For each index in `order`, appends the corresponding sample from `frame` to `out` as little-endian 4-byte ints via append_i32le, exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `decode_reference` — QUIRKY — TANGLED
- spec 3 · read at `b7eadc63697e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:07:39Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Decodes a raw DVD-Audio LPCM payload into per-channel PCM samples, following foo_input_dvda's model: it uses group1_bits/group2_bits to determine per-sample bit widths for two channel groups, ratio to interleave/step between the two groups' sample rates, and assignment to map decoded samples to output channel order (via source_order/wave_indices). It likely calls decode_group internally to unpack each group's bits into integer samples and returns them bundled in a DecodeResult.
- found: Loops over the payload decoding group1 every iteration and group2 only once every `ratio` iterations (reusing the last decoded group2 samples otherwise), producing two interleaved output frames per loop pass in both raw source order and wave (assignment-mapped) order, appended into a DecodeResult.
- predicted: some · documented: none · derivable: yes · legible: some · trap: no

### `hex`
- spec 3 · read at `7458bb261dd3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:12:41Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Converts a vector of bytes into a lowercase hex string, iterating over each byte and appending its two-hex-digit representation (via snprintf or a hex digit lookup table), used to emit reference vector output for comparison in tests.
- found: Uses ostringstream with std::hex/setfill/setw(2) to format each byte as two lowercase hex digits, concatenated into a string.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `main`
- spec 3 · read at `a94ca50441e6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:34:40Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: main() iterates over a set of test parameter combinations (channel assignment, group/bit-depth configurations), uses the Lcg PRNG to build deterministic pseudo-random raw LPCM payloads via make_payload, decodes each with decode_reference, and prints the input/expected-output pairs (likely hex-encoded, one per line or as structured text) to stdout to be captured as a fixture file for the Rust test suite.
- found: Nested loops over channel assignment × group1 bit depth × (group2 bit depth × ratio, skipping group2>group1) generate deterministic seeded payloads via make_payload, decode them with decode_reference, and print tab-separated fields (config params, hex payload, hex source/wave outputs) as a fixture text stream to stdout, with a trailing emitted-count comment.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

## tests/preemph_ablation_ladder.rs

### the file itself
- spec 3 · read at `b25a253277ad` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:09:49Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: An evaluation harness (likely a test or experiment binary) that compares five increasingly complex feature subsets (A–E, as listed in the doc) for the pre-emphasis spectral detector against a labeled corpus of albums. collect_files/album_name gather and group track audio files by album; compute_track extracts the per-track features (deemph_delta, alpha, pe_correlation, spread, etc.) for each; train_logreg/score_logreg fit and apply a small logistic-regression classifier on a chosen feature subset; evaluate_album_model pools per-track scores into an album-level decision and measures accuracy/FPR at some fixed threshold; and ablation_ladder drives the whole comparison across variants A through E, printing or asserting that detection performance (e.g., true-positive rate at a fixed false-positive rate) improves monotonically as more features are added, i.e. that the full 6-feature model outperforms simpler ablations.
- found: Matches prediction's structure closely: compute_track extracts per-track spectral features, collect_files/album_name group by album, train_logreg/score_logreg implement a from-scratch standardized logistic regression with class weighting, evaluate_album_model pools track scores to album level (median of >=3 tracks) and sweeps thresholds to find the best recall at a target FPR, and ablation_ladder drives 7 variants (A-G, more than the 5 documented) printing a results table. Missed that this is a manual/local experiment gated on ~/preemph-dev/{preemph,non-preemph} directories existing (skips entirely otherwise, not run in normal CI) and that it only prints results — there are no pass/fail assertions comparing the variants.
- predicted: most · documented: most · derivable: no · legible: not judged · trap: no
- note: The header doc lists only 5 ablation variants (A-E) but the code actually runs 7 (through G), and it's a skip-if-corpus-missing dev tool rather than an assertion-bearing test.

### `compute_track`
- spec 3 · read at `17b72810b170` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:48:58Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Loads/decodes the audio file at path, runs the pre-emphasis detector's feature extraction using corpus_model for any corpus-relative normalization, computes the track-level features referenced in the file doc (deemph_delta, median alpha, pe_correlation, spread q75-q50, and other features feeding the full 6-feature model), and packages them into Some(TrackData) — returning None if the file can't be decoded or doesn't meet some minimum length/validity requirement.
- found: Probes the audio file, rejects sample rates above 48kHz, computes band spectra via STFT, selects relevant frames (bailing if none), then runs several scoring passes (model scores, virtual deemphasis delta, multi-alpha stats, track shape features) against corpus_model, and assembles all of it into a TrackData with per-feature fields (alpha median/p75/spread, pe_correlation, frac_pos, shape, frame_count).
- predicted: most · documented: some · derivable: no · legible: most · trap: no
- note: The file doc's ablation-variant list (A-E) describes the overall test file's comparison ladder, not this function specifically, which just computes the full raw feature set for one track.

### `album_name`
- spec 3 · read at `0573dd04e95e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:12:09Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Extracts the album name from a track file path -- likely by taking the parent directory's file name (the album folder) and converting it to a String, with some fallback like "unknown" if the path has no parent.
- found: Takes the parent directory's file_name, converts to str, falling back to "?" if any step fails, and returns it as an owned String.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `collect_files` — QUIRKY
- spec 3 · read at `867bdebacdd1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:03:45Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Reads directory entries from dir, filters to files (skipping subdirectories), and returns their paths as a Vec<PathBuf>, probably sorted for deterministic ordering across test runs.
- found: Recursively walks dir with walkdir, filters to entries with a .flac extension, and collects their paths into a Vec — no sorting, no generic "is file" filter.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `train_logreg` — QUIRKY
- spec 3 · read at `2e8d000ebca7` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:34:34Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Trains a logistic regression classifier via gradient descent: initializes weights (length n_features) and bias to zero, then for a fixed number of iterations computes sigmoid predictions for each sample, accumulates gradients of the loss (with L2 regularization term scaled by lambda) with respect to weights and bias, and updates weights/bias with a learning rate. Returns final (weights, bias).
- found: Standardizes features, computes class-balancing weights for pos/neg labels, runs 500 epochs of full-batch gradient descent with L2 regularization on the standardized features, then transforms the learned weights/bias back to the original (unstandardized) feature scale before returning.
- predicted: some · documented: some · derivable: no · legible: most · trap: no

### `score_logreg`
- spec 3 · read at `2f9aa105c31f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:48:46Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Computes the logistic regression score: dot product of feature vector x with weights, adds the bias, then applies the sigmoid function (1/(1+e^-z)) to return a probability in [0,1].
- found: Computes only the raw linear logit (bias + dot product of x and weights); it does NOT apply the sigmoid, despite the name "score_logreg" suggesting a full logistic regression score/probability.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Name suggests a probability output but this returns the raw logit only; callers must apply sigmoid or threshold at 0 themselves.

### `evaluate_album_model`
- spec 3 · read at `46f19a62ff5a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:40:30Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Builds per-track feature vectors via track_feature_fn for pe_albums (label 1) and np_albums (label 0), trains a logistic regression (train_logreg, with lambda) on all tracks, then scores each album's tracks (score_logreg), pooling per-track scores into an album-level score (mean or median). It picks a decision threshold on the np_albums' pooled scores to achieve target_album_fpr, applies it to both sets, and returns (recall, fpr, n_pe_detected, n_fp). Possibly does leave-one-out or k-fold cross-validation across albums to avoid training and testing on the same data.
- found: Trains a logistic regression on all pooled track-level features (pe=true, np=false) with no train/test split, scores tracks per album, pools via median (only counting albums with >=3 tracks), sweeps candidate thresholds to find the one maximizing true positives while keeping album FPR <= target_album_fpr, then returns recall/fpr/tp/fp at that threshold.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `ablation_ladder`
- spec 3 · read at `9b767e8bfca5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:08:56Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Collects a labeled set of album audio files (collect_files), computes per-track features via compute_track for each of the 5 variants (A: deemph_delta only, B: +median alpha, C: +pe_correlation, D: +spread, E: full 6-feature model), trains a logistic regression per variant (train_logreg), scores tracks/albums (score_logreg, evaluate_album_model) at a fixed album-level false-positive rate, and prints/asserts a comparison table of true-positive rates across the variants to show how much each added feature improves detection.
- found: Skips unless local ~/preemph-dev/{preemph,non-preemph} dirs and a corpus model exist; scores every track's features via compute_track, groups by album, then runs evaluate_album_model for 7 feature-subset variants (A-G, more than the file_doc's A-E) at a fixed 5% target album FPR, printing a comparison table of recall/FP/FPR per variant.
- predicted: most · documented: some · derivable: no · legible: most · trap: no
- note: The file_doc lists only variants A-E but the code actually runs seven (A-G), and it's a local-corpus-gated dev script that silently no-ops (SKIP) in CI/normal test runs rather than a true assertion-based test.

## tests/preemph_album_pool.rs

### the file itself
- spec 3 · read at `3b3a46f129d6` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:15:16Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A test/tool file that collects corpus audio files, groups them into albums via album_name/album_key, scores each track with score_track_full, and pools per-album scores to compare two pre-emphasis classification approaches (current album classifier vs track-level shape model) via ab_ablation, reporting where they agree/disagree.
- found: A manual (non-assert) ablation test comparing two pre-emphasis album classifiers: trains and cross-validates Pipeline A (raw album-pooled features) and Pipeline B (track-level shape classifier scores pooled to album level) against a home-directory dev corpus of PE/non-PE FLAC albums, skipping if the corpus/db aren't present, then prints accuracy/FPR/precision, per-album detection tables, false positives, and a summary declaring which pipeline wins, plus a soft-rule matched-pairs comparison.
- predicted: most · documented: none · derivable: yes · legible: not judged · trap: no

### `score_track_full`
- spec 3 · read at `4d300b663fd4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:08:10Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Loads the audio file at path, extracts spectral/shape features, scores them against both the corpus model and the LDA classifier, and packages the results (e.g., score, classification label, path) into a TrackResult, returning None if the file can't be read or analyzed (e.g., too short, decode failure).
- found: Probes audio info, rejects >48kHz, computes STFT band spectra, selects frames, scores models against corpus, computes de-emphasis delta and multi-alpha, builds TrackFeatures/TrackShapeFeatures/TrackSummary via classifier, and packages everything into a TrackResult.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `album_name`
- spec 3 · read at `5ef61a9a625f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:12:13Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Extracts the album name from a track path by taking the parent directory's file name (as a lossy string), used to group tracks by album for the A/B ablation pooling comparison.
- found: Takes path.parent()'s file_name() as a &str, falling back to "?" if any step fails, and returns it as an owned String.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `album_key` — OBSCURE
- spec 3 · read at `59217a8c8c96` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:31:19Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Derives a normalized album grouping key from a track/file name — stripping the file extension, track number prefix, and any per-track suffix so that all tracks belonging to the same album collapse to the same key (used to pool per-track scores by album in the ablation test).
- found: Simply truncates the name at the first '(' character (trimming whitespace), or returns it unchanged if there's no parenthesis — a much simpler heuristic than actual filename/track-number stripping.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no

### `collect_files`
- spec 3 · read at `6e0a69cdf02c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:03:45Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Reads the given directory (likely recursively via walkdir or similar) and collects all file paths within it into a Vec<PathBuf>, probably filtering to relevant audio file extensions (like .flac/.wav) used by this A/B ablation test comparing an album classifier to a track-level model.
- found: Recursively walks the directory with walkdir, filters to .flac files, and returns their paths as a Vec<PathBuf>.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `ab_ablation`
- spec 3 · read at `1849961d71f5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:23:06Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Uses collect_files to gather test audio files, groups them by album_key/album_name, runs both the current album-level classifier and a track-level shape model via score_track_full on each track, pools the per-track scores into an album-level verdict, and prints/asserts comparison metrics (agreement rate, accuracy vs labeled ground truth) between the two approaches.
- found: Skips unless a local dev corpus dir and pretrained classifier/db exist; collects PE/non-PE track files, scores each track, trains Pipeline A (album classifier on raw album features) and Pipeline B (track shape classifier trained then pooled into album features) each via cross-validation, prints accuracy/FPR/precision for both, runs both classifiers over every album to compare detections and false positives, prints a final summary declaring which pipeline wins, then prints matched PE/non-PE album pairs (by soft album_key match) with soft-pooled confidence scores for reference.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

## tests/preemph_calibrate.rs

### the file itself
- spec 3 · read at `fc1105e17636` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:15:28Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A single end-to-end #[tokio::test] (full_calibration_pipeline) that mirrors the pattern from preemph_error_analysis.rs: loads a corpus of PE and non-PE sample audio from disk (skipping if unavailable), trains/computes an empirical PE template and calibrates an LDA classifier from it, then runs the calibrated classifier against test files and prints/asserts detection accuracy — essentially a smoke test that the full train-then-detect pipeline works end to end.
- found: Matches the header exactly: trains/loads corpus, computes empirical template if missing, calibrates an LDA classifier (printing CV accuracy/FPR/precision/threshold/weights), then runs detect_preemphasis on one PE and one non-PE sample file and prints confidence/detail. No assertions — purely a printed diagnostic pipeline, skipped if the local dev directories are absent.
- predicted: full · documented: full · derivable: no · legible: not judged · trap: no

### `full_calibration_pipeline`
- spec 3 · read at `ef1f681d9b59` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:11:30Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Loads/builds a labeled training corpus of pre-emphasized vs non-pre-emphasized audio, computes an empirical spectral template from it, calibrates an LDA classifier using that template, then runs the calibrated classifier against known sample/test files and asserts it correctly detects pre-emphasis (e.g. via accuracy assertions or per-file expected-label checks).
- found: A manual/dev-only integration test that skips entirely unless local ~/preemph-dev corpus directories exist; it lazily trains the corpus and empirical template if missing, calibrates an LDA classifier, then prints (does not assert) accuracy/FPR/precision/weights and runs detect_preemphasis on one PE and one non-PE sample file, printing their confidence/detail with no pass/fail assertions.
- predicted: most · documented: full · derivable: no · legible: full · trap: no

## tests/preemph_debug.rs

### the file itself
- spec 3 · read at `9acc402c0d91` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T07:15:28Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A diagnostic, not really assertion-driven, test file for exploring pre-emphasis (PE) detection, likely for a vinyl-rip/mastering pipeline. It probably loads or synthesizes audio samples with PE applied vs without, computes frequency spectra, uses median_spectrum to build representative average spectral shapes for PE, non-PE, and a broader "corpus" reference, then uses pearson_correlation to numerically compare a candidate spectrum's similarity to each reference shape — printing results for human inspection rather than hard-asserting pass/fail.
- found: A single #[tokio::test] that loads a precomputed corpus model plus one real PE FLAC and one real non-PE FLAC from a hardcoded local dev directory (~/preemph-dev), computes per-band STFT spectra, selects frames, takes the median spectrum per file, and prints a table comparing corpus mean, PE median, non-PE median, PE-minus-corpus diff, and the theoretical PE gain curve per frequency band, ending with a single Pearson correlation between the PE-corpus diff and the theoretical curve — purely for human eyeballing via println, with no assertions.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: This test reads from a hardcoded home-directory path (~/preemph-dev) that won't exist in CI or on other machines, so it will panic (unwrap on WalkDir results) rather than skip outside the original author's machine.

### `debug_spectral_shapes`
- spec 3 · read at `3d73a1dcfddc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:14:10Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: An ad-hoc debug test that loads/decodes sample audio files (pre-emphasized vs non-pre-emphasized) plus a corpus mean, computes each spectrum via median_spectrum, and compares them pairwise using pearson_correlation, printing the results so a human can eyeball how distinguishable PE vs non-PE spectral shapes are.
- found: Loads a corpus mean spectrum model, picks the first FLAC in a PE dir and first in a non-PE dir, computes each file's median band spectrum via STFT+frame-selection, prints a table comparing corpus/PE/nonPE/diff/theoretical-PE-curve per band, then computes and prints the Pearson correlation between (PE - corpus) and the theoretical pre-emphasis gain curve — not a pairwise comparison among all three spectra as I predicted.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Only one correlation is computed (PE-vs-corpus diff against the theoretical PE curve), not pairwise correlations among corpus/PE/non-PE as the description alone suggests.

### `median_spectrum`
- spec 3 · read at `07ae69b9ae76` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:11:55Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: For each frequency band, collects the magnitude/energy values across all selected frames from the StftResult, sorts them, and takes the median (middle value or average of two middles), storing results into a NUM_BANDS-sized array. Used as a debug/statistical summary of spectral shape robust to outlier frames.
- found: Per frequency band, collects magnitude values across selected frame indices, sorts them, and computes the median (middle value or average of two middle values); returns zeroed array if no frames selected.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `pearson_correlation`
- spec 3 · read at `4906a80c705e` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:28:43Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Computes the standard Pearson correlation coefficient between two equal-length f64 slices: computes means of x and y, then sums (xi-mean_x)*(yi-mean_y) for covariance and the sums of squared deviations for each, and returns covariance divided by the product of the two standard deviations (sqrt of sum of squares).
- found: Standard Pearson correlation: means, then covariance and sum-of-squares in a single loop, divided by sqrt(dx2*dy2) with a zero-variance guard returning 0.0 instead of dividing by zero/NaN.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

## tests/preemph_empirical.rs

### the file itself
- spec 3 · read at `9b06dca87a2e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:15:53Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Test file validating a pre-emphasis (RIAA/AAD-era) detection algorithm using real, empirically-derived data rather than synthetic signals. Loads paired sample files (pre-emphasized vs. de-emphasized versions of the same source) via sample_files, builds/uses an empirical spectral template from them, and scores candidate files against that template via score_file, using pearson correlation and a quad_form (quadratic-form/Mahalanobis-style) distance metric to produce a diagnostic comparison, asserting the empirical template correctly separates pre-emphasized from non-pre-emphasized files, plus extra diagnostic output requested during development/review.
- found: A manual diagnostic script (not an assertive test — no assert!, just prints) gated on local dev directories (~/preemph-dev/{preemph,deemphasized,non-preemph}) existing, else it skips. It trains/loads a corpus model, computes an empirical PE (pre-emphasis) spectral template from paired PE/deemphasized files, compares it to a theoretical template via Pearson correlation and a covariance quadratic form, then scores sampled files from all three categories (PE, deemphasized, non-PE) printing mean/min/max z-score, alpha, correlation, and deemphasis delta per group for human inspection.
- predicted: most · documented: most · derivable: no · legible: not judged · trap: no
- note: Not primed by this repo's own CLAUDE.md.

### `score_file` — QUIRKY
- spec 3 · read at `5c56e3aade31` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:13:53Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Loads the audio file at `path`, extracts spectral envelope frames, and computes several diagnostic scores against the corpus/empirical PE template — likely a Mahalanobis-style distance (via quad_form), a Pearson correlation against the empirical PE template, and one or two other summary statistics — returning them alongside the frame count as a 5-tuple, with Err(String) on load/analysis failure.
- found: Probes the file, rejects hi-res, loads the corpus model, computes STFT band spectra, selects frames, scores the models (z_score, alpha, pe_correlation) and a virtual deemphasis delta, returning those plus frame count.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `sample_files`
- spec 3 · read at `1ec90d4b413d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:41:22Z · by ross@rossturk.com · warm reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Same as the earlier sample_files in tests/preemph_threeway.rs: recursively walks the directory with walkdir, filters to .flac files, sorts them, and if more than max, evenly steps through the sorted list to sample up to max files.
- found: Identical to tests/preemph_threeway.rs's sample_files: walkdir recursive scan filtered to .flac, sorted, evenly step-sampled up to max.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no
- note: This is a byte-for-byte duplicate of tests/preemph_threeway.rs#sample_files — likely copy-pasted across the two calibration test files rather than shared via a common test helper module.

### `empirical_template_test`
- spec 3 · read at `db43b3fa05a8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:54:27Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Loads paired PE/deemphasized sample audio files via sample_files, computes an empirical pre-emphasis correction template from the paired differences, then runs diagnostic scoring using score_file, pearson correlation, and quad_form against the template, asserting the metrics meet expected thresholds.
- found: Skips if fixture dirs are missing; ensures/trains a corpus model, computes an empirical PE template via corpus::train_empirical_template, compares it to a theoretical curve (pearson correlation, quad_form covariance diagnostics) and prints per-band values, then scores sampled PE/deemphasized/non-PE files with score_file and prints summary statistics — no assertions, purely diagnostic output.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: This is a manual diagnostic harness gated on local ~/preemph-dev fixture directories, not a CI-run assertion test — it silently no-ops (SKIP) when those directories aren't present.

### `pearson`
- spec 3 · read at `9df3d0269b77` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:33:59Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Computes the Pearson correlation coefficient between two equal-length f64 slices x and y: calculates their means, then sums (x_i - mean_x)*(y_i - mean_y) divided by the sqrt of the product of sum of squared deviations, returning a value between -1 and 1.
- found: Standard Pearson correlation coefficient computation, with a guard returning 0.0 if either variance is zero to avoid division by zero.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `quad_form`
- spec 3 · read at `707f99cd79b0` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:28:39Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Treats cov as a flattened NUM_BANDS x NUM_BANDS matrix and computes the quadratic form s^T * cov * s, but restricted to only the indices where mask[i] is true — i.e. double sum over masked i,j of s[i] * cov[i*NUM_BANDS+j] * s[j], skipping unmasked entries.
- found: Builds list of masked indices, then double-sums s[si]*cov[i*NUM_BANDS+j]*s[sj] where s is already a compact vector over just the masked entries (indexed by position in the masked-index list, not the full band index), with out-of-range cov lookups defaulting to 0.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

## tests/preemph_error_analysis.rs

### the file itself
- spec 3 · read at `709e0b7f229e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:15:17Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A diagnostic test file (not a typical pass/fail assertion suite) for a pre-emphasis (PE) detection algorithm, specifically investigating false negatives — albums with real pre-emphasis that the detector fails to flag. It loads/iterates known PE albums, runs per-track diagnostics (diagnose_track) using a masked PE template and per-frame alpha computation, splits results into detected vs missed groups, and prints comparative summaries (print_album_summary, print_aggregate) of feature distributions, frame selection, and aggregation sensitivity to help a human understand why the 23 missed albums are missed.
- found: A #[tokio::test] that loads a real PE (pre-emphasis) audio corpus from disk (skips if absent), scores every track, classifies albums as detected/missed via a median-alpha + fraction-positive heuristic, then prints comparative summaries and runs two ablations (all-frames alpha, top-quartile alpha) to see how many missed albums would be recovered under alternate frame-selection/aggregation strategies.
- predicted: full · documented: most · derivable: no · legible: full · trap: no
- note: The 2-line file header only states the goal ('why 23 albums missed'); it doesn't mention the ablation experiments or the on-disk corpus dependency (test silently SKIPs if ~/preemph-dev/preemph is absent), which are the bulk of the file.

### `diagnose_track` — QUIRKY
- spec 3 · read at `9d85d971cf1d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:20:54Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: diagnose_track opens/decodes the track at path, computes per-frame alpha values (via compute_per_frame_alphas) and other pre-emphasis-detection features, compares them against corpus_model to see how confidently the track would be classified, and packages the results into a TrackDiag struct — returning None if the file can't be loaded or decoded.
- found: Probes the audio file (skipping if sample rate > 48kHz), computes band spectra via STFT, selects quiet frames, scores against the corpus model for alpha/correlation/stability/deemphasis-delta, then separately computes per-frame alpha distributions for both the quiet-frame selection and all frames, deriving fraction-positive, p75, top-quartile mean, and all-frames median — packaging everything into a TrackDiag for forensic comparison of detected vs missed albums.
- predicted: some · documented: none · derivable: no · legible: most · trap: no

### `get_pe_template_masked`
- spec 3 · read at `2d76e5398b6f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:48:16Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Checks corpus_model for an empirical PE template and uses it if present, otherwise falls back to a theoretical template; then applies the mask array to zero out (or exclude) unusable bands and returns the resulting Vec<f64>.
- found: Uses corpus_model's empirical PE template if present, else falls back to models::pe_curve(), then filters values to only masked-true bands via zip/filter/map, returning a Vec<f64>.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `compute_per_frame_alphas`
- spec 3 · read at `9cc51fd688fd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:53:32Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: For each frame index, extracts that frame's per-band spectral values from stft_result, applies mask to select relevant bands (masked by corpus_model context), fits and subtracts an intercept and linear tilt (least-squares against band index/frequency) to detrend the spectrum, then projects the detrended residual onto pe_template (dot product normalized by the template's squared norm) to yield a per-frame alpha scalar representing PE-template strength, collecting these into a Vec<f64> in frame_indices order.
- found: For each frame, subtracts corpus_model.mean from the band spectrum, masks to selected bands, does a linear least-squares fit against band index to remove intercept+tilt, then computes alpha as the dot product of the residual with pe_template divided by the template's squared norm (a least-squares projection coefficient), skipping frames with fewer than 2 masked bands.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The doc line only describes the projection step, not the preceding subtraction of corpus_model.mean, which is a nontrivial detail not derivable from the signature alone.

### `album_name`
- spec 3 · read at `988c17d2b244` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:48:52Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Extracts a human-readable album name from a file path — likely the parent directory's file name (or last path component before the track file), falling back to the full path string if parsing fails, used for grouping/labeling in the printed error-analysis report.
- found: Returns the parent directory's file name as a String, or "unknown" if any step fails.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `error_analysis` — QUIRKY
- spec 3 · read at `1b0fd2a0a838` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:15:55Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Loads a hardcoded list of pre-emphasis (PE) albums split into detected and missed groups, runs diagnose_track/compute_per_frame_alphas/get_pe_template_masked on each track to gather per-track feature distributions and frame-selection stats, then prints per-album summaries and an aggregate comparison (via print_album_summary/print_aggregate) to reveal what distinguishes the missed albums from detected ones — likely an ignored diagnostic test rather than an asserting one.
- found: Skips unless a local dev corpus dir exists; scores every PE flac track, classifies albums as detected/missed via a median-alpha and fraction-positive threshold rule (not a hardcoded list), prints per-album and aggregate stat comparisons, then runs two ablations (using all-frame alpha, and p75/top-quartile alpha instead of quiet-frame alpha) to see how many missed albums would be recovered under each alternative scoring rule.
- predicted: some · documented: most · derivable: no · legible: most · trap: no

### `print_album_summary` — QUIRKY
- spec 3 · read at `2321a1fb9752` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:02:38Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Prints a header with the album name, then iterates over the tracks slice printing per-track diagnostic fields (likely something like alpha estimate, confidence/score, and detected-vs-expected status) in a formatted table row. Probably also tallies how many tracks were correctly detected vs missed within the album and prints a short summary line/count at the end.
- found: Computes median alpha, mean alpha across three different aggregation strategies (p75, top-quartile, all-frames), fraction of tracks with positive alpha, and mean PE correlation across the album's tracks, then prints them as one formatted table row (truncating the album name to 53 chars).
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `print_aggregate`
- spec 3 · read at `38100f48c0b5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:41:47Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Aggregates track-level diagnostic features (e.g. alpha values, frame-selection stats) across all tracks in the given albums, computes summary statistics (mean/median/min/max) per feature, and prints them to stdout prefixed with label — used to compare "detected" vs "missed" pre-emphasis album groups side by side in the forensic analysis test output.
- found: Flattens all tracks across the given albums, computes mean values for several alpha-related fields (alpha, alpha_p75, alpha_top_quartile, alpha_all_frames), fraction with positive alpha, mean pe_correlation, and mean frame count, then prints a two/three-line summary block prefixed with label.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

## tests/preemph_fold_debug.rs

### the file itself
- spec 3 · read at `1addb6af7937` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:09:33Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A lightweight diagnostic test (single function debug_fold_assignment) that scans the same kind of local dev PE/non-PE audio directories used elsewhere in this test suite, groups files by some key (e.g. artist/album/source) to avoid data leakage across cross-validation folds, and prints out the resulting group sizes and fold assignments purely from file paths/metadata — with no audio decoding or spectral analysis — to sanity-check that folds are balanced and grouping logic is correct before running the expensive spectral tests.
- found: Matches the core prediction (scans the dev PE/non-PE dirs, groups by album_group_id, prints group/fold distribution, no real audio decoding). Missed specifics: it fabricates synthetic/dummy feature vectors via a manual PRNG instead of just inspecting file paths, checks an "eligibility" filter on those dummy features, manually simulates round-robin fold assignment, and also actually invokes grouped_cv_train_with_calibration_report across several k values as a smoke test — so it's not purely metadata-only as I predicted, it does exercise the CV/training code path with fake features.
- predicted: most · documented: none · derivable: yes · legible: not judged · trap: no

### `debug_fold_assignment` — TANGLED
- spec 3 · read at `8ce7d650e683` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:43:35Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A manual debug test (not really assertion-driven) that builds a synthetic multi-track/multi-group disc or job structure and runs whatever logic assigns tracks to pre-emphasis detection "folds"/groups, then prints (println!) the resulting group distribution and fold assignments for a human to eyeball, explicitly skipping actual audio decoding/processing to keep it fast.
- found: Walks real FLAC files from ~/preemph-dev/{preemph,non-preemph} directories, generates dummy pseudo-random TrackFeatures per file (seeded LCG), groups them by album_group_id, prints PE/non-PE counts per group, computes eligibility per the scoring module's alpha/frame/stability thresholds, manually simulates round-robin group-stratified 3-fold assignment, and then actually runs scoring::grouped_cv_train_with_calibration_report for k in [2,3,4,5], printing accuracy/FPR/threshold or failure per fold count.
- predicted: most · documented: none · derivable: yes · legible: some · trap: no

## tests/preemph_full_calibration.rs

### the file itself
- spec 3 · read at `ad6f08e02744` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:15:38Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: A corpus-based calibration test for a pre-emphasis (PE) detection heuristic, likely gated behind #[ignore] or an env var since it needs a real training corpus on disk. score_file_spectral computes a spectral PE-likelihood score for one audio file; album_key derives a grouping key (album/release name) from a file path so PE and non-PE versions of the same recording can be paired; full_calibration scores every PE and non-PE file in the corpus, reports the statistical separation between the two score distributions, and does matched-pair comparisons (same album, PE vs non-PE) to validate the detector's threshold.
- found: Matches prediction closely: trains a PCA-based corpus model from non-PE files, scores every PE/non-PE FLAC file in two home-dir corpora via an LLR (log-likelihood-ratio) + alpha score using a deprecated legacy-alpha verdict function, prints per-file scores, reports separation gap (PE min vs non-PE max LLR) and per-album matched-pair mean deltas via album_key grouping. Skips (not #[ignore]) if the corpus directories aren't present on disk rather than being gated by an attribute.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Uses #[allow(deprecated)] to call compute_verdict_legacy_alpha — worth knowing there's a newer non-legacy verdict path this calibration test deliberately isn't exercising.

### `score_file_spectral`
- spec 3 · read at `0362aec65e8a` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:44:39Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Decodes the audio file at `path`, computes a pre-emphasis spectral score (likely a high-frequency energy ratio or similar metric) over the samples, and returns Ok((score, some secondary numeric metric, a count such as sample or channel count, an album key string derived from the path)) on success, or Err(String) with a descriptive message if the file can't be read/decoded.
- found: Probes the audio file, rejects sample rates above 48kHz, loads a corpus model, computes STFT band spectra, selects qualifying frames, scores multiple pre-emphasis detection models against the corpus, computes a virtual de-emphasis delta, then a legacy-alpha verdict, returning (verdict.score, model_scores.alpha, frame count, verdict.confidence as string).
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `album_key` — QUIRKY
- spec 3 · read at `e3d22be332f4` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:11:11Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Extracts the album directory/file name from the path, then strips a trailing {...} bracketed segment (pressing info like {SHM-CD} or {24bit}) using string splitting, returning the remaining base name so that PE and non-PE versions of the same album can be matched by equal keys.
- found: Gets the file's parent directory name as a string, then truncates it at the first '(' (dropping year/format/pressing info) and trims whitespace, returning "Artist - Album" as the key; falls back to the full parent name if no '(' is present.
- predicted: some · documented: some · derivable: no · legible: full · trap: no

### `full_calibration`
- spec 3 · read at `2f6ea4feba01` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:52:53Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Walks a training corpus of known pre-emphasis and non-pre-emphasis audio files, computes a spectral score for each via score_file_spectral, groups results by album (album_key) to build matched PE/non-PE pairs, prints/reports separation statistics between the two populations, and asserts some minimum separation or classification accuracy so future spectral-detector changes can't silently regress.
- found: Skips if local dev corpus directories aren't present. Trains a corpus model from non-PE files, then scores every PE and non-PE flac file (via score_file_spectral on blocking tasks), printing per-file LLR/alpha, then prints summary stats (mean/min/max) for each group, computes and prints a separation gap between PE-min and non-PE-max, and finally groups by album_key to print matched-pair PE-vs-non-PE deltas per album — but it's purely diagnostic output, not an assertion-gated regression test (only panics if corpus training itself fails).
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: I assumed it asserted a minimum separation threshold as a pass/fail gate; it actually only prints diagnostics and never fails on poor separation.

## tests/preemph_pipeline.rs

### the file itself
- spec 3 · read at `6e83f113b15a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:15:26Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Integration test file with helper functions find_pe_file/find_non_pe_file that locate known sample files in the ~/preemph-dev corpus, and separate test functions test_corpus_training, test_detect_pe_file, test_detect_non_pe_file that each skip if the corpus is missing, otherwise train/load the corpus model and assert correct detection results and reasonable spectral scores.
- found: Exactly as documented: helpers find sample PE/non-PE FLAC files under ~/preemph-dev, and three async tests train the corpus (asserting minimum track/frame/PCA counts), detect a known PE file (asserting Detected confidence with metadata confirmation), and detect a known non-PE file (asserting it's not flagged Detected/StrongCandidate), all skipping gracefully when the dev corpus directory is absent.
- predicted: full · documented: full · derivable: no · legible: not judged · trap: no

### `find_non_pe_file`
- spec 3 · read at `d939fa1d79a2` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T05:55:16Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Reads the ~/preemph-dev/non-deemph/ directory, iterates its entries, and returns the first (or some) audio file found as Some(PathBuf); returns None if the directory doesn't exist or contains no suitable files.
- found: Resolves ~/preemph-dev/non-deemph, recursively walks it with walkdir, and returns the first .flac file found; None if home dir or no such file exists.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `find_pe_file`
- spec 3 · read at `cea904d0fa46` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T05:50:16Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Looks in a hardcoded dev directory like ~/preemph-dev (or a subfolder for known pre-emphasized files), reads its entries, and returns the path of the first file found (or a specific known-good PE file), returning None if the directory doesn't exist or is empty — used to skip the test gracefully when the dev corpus isn't present locally.
- found: Uses dirs::home_dir() joined with "preemph-dev/preemph", walks it recursively with walkdir, and returns the first .flac file found, or None if the dir/home doesn't exist.
- predicted: most · documented: some · derivable: no · legible: full · trap: no
- note: Subfolder is specifically "preemph" (not a generic dev root) and it filters by .flac extension via recursive walkdir rather than a flat directory listing.

### `test_corpus_training`
- spec 3 · read at `6cb99fe34d44` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:18:03Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Async integration test that trains the spectral scorer using audio files found under ~/preemph-dev/non-deemph/ (likely skipping/early-returning if the directory doesn't exist on this machine), then asserts the resulting trained corpus/model has sane properties, such as a nonzero file count or reasonable statistics.
- found: Skips if the corpus dir is missing; otherwise trains via train_corpus_from_dir and asserts the model has at least 30 tracks, at least 100 frames, and non-empty PCA components, panicking on error.
- predicted: most · documented: full · derivable: no · legible: full · trap: no

### `test_detect_pe_file`
- spec 3 · read at `a7b3a198263c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:43:06Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Uses find_pe_file() to locate a known pre-emphasized test file, runs the pre-emphasis detector/pipeline on it asynchronously, and asserts the result indicates pre-emphasis was detected, likely checking that detection came from metadata rather than requiring the spectral scorer.
- found: Skips (returns early with eprintln) if find_pe_file() finds nothing, otherwise calls detect_preemphasis on it and asserts confidence == Detected and cue_confirmed == true, i.e. metadata-based detection.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `test_detect_non_pe_file`
- spec 3 · read at `7bed0fc9e9b3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:49:54Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Uses the find_non_pe_file helper to locate a known non-pre-emphasized test file, runs the detection pipeline (metadata check + possibly spectral scoring) on it, and asserts that the result reports it as NOT pre-emphasized — checking a boolean/enum field like is_pe == false or a low confidence/PE-likelihood score, similar in structure to its test_detect_pe_file counterpart but asserting the opposite outcome.
- found: Skips (with eprintln) if the training directory or a non-PE test file isn't available on disk, trains the corpus on demand if not already loaded, runs detect_preemphasis on the chosen file, prints detailed diagnostics, and asserts confidence is neither Detected nor StrongCandidate.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

## tests/preemph_spectral_only.rs

### the file itself — QUIRKY
- spec 3 · read at `37490d23429c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:09:08Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Integration test that exercises the pre-emphasis spectral scorer directly (skipping tag/catalog-based detection), likely by synthesizing or loading fixture audio with and without simulated pre-emphasis applied, running compute_band_spectra + the M0/M1/M2 model-comparison scorer via score_file_spectral, and asserting in test_spectral_pe_vs_non_pe that the scorer's output correctly discriminates PE from non-PE audio based purely on spectral tilt/shape.
- found: Confirms the general shape (score_file_spectral pipes a file through stft/frame_select/models/scoring, bypassing tags), but I was wrong on key details: it's not a fixture-based test but reads real developer-local audio files from ~/preemph-dev/{preemph,non-preemph} and skips entirely if that directory or the corpus model is missing; and rather than asserting pass/fail on separation, it only prints diagnostic LLR statistics and a qualitative "CLEAN SEPARATION" vs "OVERLAP" message with no actual test assertion.
- predicted: some · documented: none · derivable: yes · legible: not judged · trap: no

### `score_file_spectral` — QUIRKY
- spec 3 · read at `98ebcb35bc7e` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:50:41Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Reads/decodes the audio file at path (likely via a WAV reader), runs it through the spectral M0/M1/M2 model comparison scorer directly (skipping any metadata-based pre-emphasis detection), and returns (llr, alpha, frames_scored, confidence_label) — propagating any I/O or decode error as a String.
- found: Probes the file for sample rate, loads a reference corpus model, computes band spectra via STFT, selects qualifying frames (erroring if none), scores M0/M1/M2 models against the corpus, computes a virtual de-emphasis delta score, then combines everything via a deprecated legacy-alpha verdict function to produce the final (score, alpha, frame_count, confidence) tuple.
- predicted: some · documented: most · derivable: no · legible: most · trap: no
- note: Uses an #[allow(deprecated)] legacy alpha verdict function rather than whatever the current non-legacy path is — a reader wouldn't know from the signature that this test deliberately exercises deprecated scoring code.

### `test_spectral_pe_vs_non_pe` — QUIRKY — TRAP
- spec 3 · read at `bb1be312e550` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:02:12Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: An async test that takes or synthesizes a set of pre-emphasized (PE) and non-pre-emphasized audio fixture files, runs each through score_file_spectral to get its M0/M1/M2 spectral model comparison score, and asserts PE files consistently score as "pre-emphasized" while non-PE files score as "flat", verifying the spectral-only scorer separates the two classes without relying on file metadata.
- found: Not really an assertion-based test: it skips (early return) if a trained corpus model or the ~/preemph-dev/{preemph,non-preemph} directories aren't present, otherwise scores up to 5 PE and 5 non-PE FLAC files with score_file_spectral, and just prints per-file LLR/alpha/frames/confidence plus a mean/range summary and a "CLEAN SEPARATION" vs "OVERLAP" message — there is no assert! anywhere, so the test always passes as long as it doesn't panic, regardless of separation.
- predicted: some · documented: most · derivable: no · legible: most · trap: yes
- note: This function name and file_doc read like a pass/fail correctness test, but it contains zero assertions and depends on an external, non-repo directory (~/preemph-dev/...) that will be absent in CI, so it silently no-ops there — a future reader relying on "this test verifies separation" would be wrong.

## tests/preemph_threeway.rs

### the file itself
- spec 3 · read at `986ccbd9fd56` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:14:38Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: An integration test/analysis harness for pre-emphasis (PE) detection: it scores a set of sample audio files across three categories (PE, deemphasized, and non-PE) using some spectral-signature scoring function (score_file), gathers file paths per category (sample_files), and runs a three-way comparison (threeway_comparison) to validate the hypothesis that deemphasized files score similarly to non-PE files while PE files stand apart. It's likely more of a diagnostic/calibration script (printing scores, maybe soft assertions) than a strict pass/fail unit test, given the "Key hypothesis" framing.
- found: A manual diagnostic test (skips via eprintln+return if local ~/preemph-dev sample directories are absent) that trains a preemphasis-detection corpus on non-PE files, scores up to 50 sampled FLAC files from each of three groups (PE, deemphasized, non-PE) via score_file, and prints a summary table plus a qualitative check of whether deemphasized files' alpha score clusters closer to non-PE than to PE.
- predicted: most · documented: full · derivable: no · legible: not judged · trap: no
- note: This is a local-machine-only calibration script, not a CI-portable test — it hardcodes a home-directory dataset path and silently skips when absent, and its only 'assertion' is a printed checkmark/warning, not a panic.

### `score_file` — QUIRKY
- spec 3 · read at `48d43209b400` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:51:23Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Decodes the audio file at the given path, extracts the relevant spectral features, and runs them through the PE classifier/scoring logic to produce a tuple of (score, and two other numeric metrics like confidence/margin, plus a count such as number of frames/samples analyzed). Returns Err(String) if the file can't be loaded or decoded, for use in the threeway comparison test.
- found: Probes the audio file, rejects hi-res (>48kHz) files, loads a shared corpus model, computes band spectra via STFT, selects frames, scores models against the corpus, computes a deemphasis delta score, and returns (z_score, alpha, pe_correlation, frame_count) as a tuple — the legacy verdict computation is called but discarded.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `sample_files`
- spec 3 · read at `7fb16777152b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:40:45Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Reads all files in the directory, collects sorted paths, and if there are more than max, picks max evenly-spaced indices across the sorted list rather than just taking the first N; returns the resulting Vec<PathBuf>.
- found: Recursively walks the directory with walkdir, filters to .flac files, sorts paths, and if more than max, evenly steps through by files.len()/max to sample up to max evenly-spaced files.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `threeway_comparison` — TRAP
- spec 3 · read at `7dc848449227` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:48:54Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Loads sample audio files across PE, deemphasized, and non-PE categories via sample_files, scores each with score_file to get a spectral metric, then compares the groups (likely averaging per-category) and asserts/prints that deemphasized scores cluster near non-PE scores while PE scores are distinctly different, validating the file doc's hypothesis.
- found: Skips entirely if dev-only sample directories aren't present on disk; otherwise trains a corpus on non-PE files, scores 50 sampled files from each of the three groups, prints a summary table of LLR/alpha/correlation stats, then prints (not asserts) whether the deemphasized group's alpha mean clusters closer to non-PE than to PE.
- predicted: most · documented: most · derivable: no · legible: most · trap: yes
- note: This is not really a CI test — it silently no-ops unless a developer-specific ~/preemph-dev directory exists, and even when it runs it never asserts anything, just prints a qualitative verdict to stdout; a reader expecting a pass/fail check would be misled.

## tests/preemph_threeway_ablation.rs

### the file itself
- spec 3 · read at `74e5885b75dd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:15:40Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Another disk-corpus diagnostic test (like preemph_error_analysis.rs), this time loading three groups of albums — PE, deemphasized, and non-PE — and computing several alpha summary variants per track (median, p75, top-quartile, all-frame) via compute_multi_summary/compute_per_frame_alphas, aggregating to album level (album_aggregate), then printing/comparing how well each summary statistic separates the three classes (compute_alpha_stats) to determine which aggregation method best discriminates PE from non-PE/deemphasized. Skips if the corpus directories aren't present locally, similar to the other preemph test files.
- found: Loads PE/deemph/non-PE corpora, computes multiple alpha summary variants per track, aggregates to album level (min 3 tracks), then prints per-statistic distribution tables, a PE-vs-non-PE separation/overlap analysis, and detection-rate comparisons across three candidate summary statistics.
- predicted: full · documented: most · derivable: no · legible: not judged · trap: no
- note: Header doesn't mention the separation-overlap and detection-rate sections, which are as large as the summary table it does describe.

### `compute_multi_summary` — QUIRKY
- spec 3 · read at `53080a805d05` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:58:08Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Loads/decodes the audio file at `path`, computes per-frame alpha (pre-emphasis) values against corpus_model (likely calling compute_per_frame_alphas), and then derives multiple summary statistics from that per-frame array: median, P75, mean of the top quartile, and mean of all frames. Packs these into a TrackMultiSummary and returns Some(...), or None if the file fails to load or yields no valid frames.
- found: Probes the audio file, rejects sample rates over 48kHz, computes band spectra via STFT, selects frames, scores against the corpus model, and computes per-frame alpha stats for both a 'quiet frame' subset and all frames, packing medians/P75/top-quartile/fraction-positive plus PE correlation and deemphasis delta into a TrackMultiSummary.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: Docs only described the file/module purpose, not this specific function's actual pipeline (probe -> STFT -> frame select -> two alpha computations -> stats).

### `compute_alpha_stats`
- spec 3 · read at `b936461bd8fc` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:53:39Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Sorts a copy of the alphas slice, then computes the median (middle value), the 75th percentile value, the mean of the values in the top quartile (top 25%), and the fraction of values that are positive (>0). Returns these four numbers as a tuple in that order.
- found: Sorts a copy of the alphas, computes median, 75th percentile value, mean of the top quartile, and fraction of values >0, with an early return of all zeros for an empty slice.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `get_pe_template_masked` — TRAP
- spec 3 · read at `65a7df7ce3e7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:48:25Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Identical duplicate of the same-named function in preemph_error_analysis.rs: takes the empirical PE template from corpus_model if present, else falls back to the theoretical curve, then filters the values to only the masked-true bands and returns a Vec<f64>.
- found: Byte-for-byte duplicate of preemph_error_analysis.rs#get_pe_template_masked: uses empirical PE template if present else models::pe_curve(), then filters to masked-true bands.
- predicted: full · documented: none · derivable: yes · legible: full · trap: yes
- note: Exact duplicate logic copy-pasted across two test files (preemph_error_analysis.rs and preemph_threeway_ablation.rs) with no shared helper module — a future change to pe template masking logic must be made in both places or they silently diverge.

### `compute_per_frame_alphas`
- spec 3 · read at `b7d0c82b4eec` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:05:29Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: For each frame index, extracts the frame's spectral values from stft_result, applies the mask to select relevant frequency bands, and fits/estimates an "alpha" scalar (pre-emphasis strength) by comparing the masked spectrum against pe_template and corpus_model (e.g. via a least-squares or ratio fit), returning one alpha per frame in the same order as frame_indices.
- found: For each frame, subtracts the corpus mean per band, masks to selected bands, linearly detrends that masked diff via least-squares regression against band index, then projects the residual onto pe_template (dot product ratio) to get an alpha scalar per frame, skipping frames with fewer than 2 masked bands.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `album_name`
- spec 3 · read at `3f661b975f9e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:12:18Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Identical to the sibling ablation test's album_name: takes path.parent().file_name() as a str, falling back to "?" if unavailable, returned as an owned String.
- found: Byte-for-byte identical to tests/preemph_ablation_ladder.rs's album_name -- duplicated test helper, not shared via a common module.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `collect_files` — QUIRKY
- spec 3 · read at `f69d0a8e6d91` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:03:47Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Simple utility: reads entries in dir (likely non-recursively) and collects them into a sorted Vec<PathBuf>, probably filtering to files only (skipping subdirectories) or to a specific extension, for deterministic ordering in the ablation test.
- found: Recursively walks dir via walkdir::WalkDir, filters to entries with a .flac extension, and collects their paths into a Vec<PathBuf>. No sorting.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `album_aggregate` — QUIRKY
- spec 3 · read at `4a8f225f704d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:38:07Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Returns None if tracks is empty; otherwise averages (or takes the mean/median of) each of the four summary types (median, P75, top-quartile, all-frame) across all tracks in the album and packages the results into an AlbumStats struct.
- found: Requires at least 3 tracks (not just nonempty), takes the MEDIAN (not mean) of several specific alpha/correlation fields across tracks via a local med() helper, and also computes a fraction-positive statistic (frac_tracks_quiet_p75_positive) I did not predict; my guess about which fields/summary-types were aggregated was only loosely right.
- predicted: some · documented: some · derivable: no · legible: most · trap: no

### `threeway_summary_ablation`
- spec 3 · read at `133b30762b5e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:14:05Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Collects audio files for PE, deemphasized, and non-PE album groups, computes per-frame alpha values against a PE template for each track, derives multiple track-level summaries (median, P75, top-quartile, all-frame) via compute_multi_summary, aggregates per album, and prints a comparison report of how well each summary type separates the three groups — an ablation/diagnostic run rather than a strict assertion-based test.
- found: Skips (with eprintln SKIP) if local dev directories or corpus model are missing; otherwise scores every file in PE/Deemph/Non-PE dirs via compute_multi_summary, aggregates to per-album stats, then prints three reports: per-statistic group summary table (mean/median/min/max/P25/P75), PE-vs-Non-PE separation/overlap analysis per candidate statistic, and simple threshold-based detection rates (PE detected, Non-PE false-positive, Deemph detected) for a few candidate statistics — purely a diagnostic printout, no assertions.
- predicted: most · documented: most · derivable: no · legible: most · trap: no

## tests/preemph_veto_experiment.rs

### the file itself
- spec 3 · read at `8b972df8e009` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:09:03Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: This is a standalone offline experiment/eval harness (likely #[ignore]-gated, run manually against a labeled corpus) comparing four PE-detector configurations (A: deemph-delta-only, B: current two-stage, C: two-stage+veto, D: stacked meta-model) using album-grouped cross-validation to avoid leakage. It defines AlbumData with aggregated per-album statistics (medians, IQR, usable-track fraction), a minimal logistic-regression trainer/scorer (train_logreg/score_lr) for the veto and meta models, per-track and per-album scoring functions for detectors A and B, a veto_features extractor, threshold tuning, corpus file/album collection helpers, and a top-level veto_experiment function that runs the whole comparison and reports whether the veto model removes false positives without hurting recall.
- found: Matches prediction closely: an async #[tokio::test] harness that runs 3-fold grouped-by-album CV comparing detectors A/B/C/D using a hand-rolled logistic regression, threshold tuning at fixed FPR, and prints a recommendation of which system to ship — skips silently if the labeled corpus directories aren't present on disk.
- predicted: full · documented: full · derivable: no · legible: not judged · trap: no

### `compute_track`
- spec 3 · read at `0d09ca96ce38` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:27:14Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Loads/decodes the audio file at `path`, runs the pre-emphasis detection analysis against it using the corpus model `cm` (computing things like deemph delta, alpha, pe correlation), and packages the resulting per-track features into a TrackData struct; returns None if the file fails to load/decode or doesn't produce usable data.
- found: Probes the audio file, bails if sample rate exceeds 48kHz or frame selection is empty, then computes band spectra, model scores, deemphasis score, and multi-alpha stats, packaging them plus a derived TrackShapeFeatures into a TrackData struct.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `deemph_median`
- spec 3 · read at `3c89c4b9ed02` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:01:31Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Collects the per-track "deemph delta" values stored on AlbumData (mirroring alpha_median/pe_corr_median), sorts them, and returns the median value as an f64.
- found: Collects deemph_delta from all tracks, sorts them, and returns the element at the middle index (v[len/2]) as a simple median, or NaN if there are no tracks.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `alpha_median`
- spec 3 · read at `72f5d96897bc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:49:08Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Computes the median of the per-track "alpha" values stored on this AlbumData, mirroring sibling methods like deemph_median and pe_corr_median which compute medians of their respective per-track metrics.
- found: Collects per-track alpha values, sorts them, and returns the element at len/2 as the median (NaN if empty) — a simple midpoint index rather than averaging the two middle values for even-length arrays.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `pe_corr_median`
- spec 3 · read at `7b1b152a3b12` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:06:33Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Collects the pe_corr (pre-emphasis correlation) field from each track in self.tracks (or similar), sorts the values, and returns the median, following the same pattern as sibling methods deemph_median and alpha_median which aggregate a per-track statistic across the album.
- found: Collects pe_correlation from each track, sorts, and returns the middle element (NAN if empty) as a simple (non-interpolated, even-length-biased) median.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `frac_pos_alpha`
- spec 3 · read at `cf19c7e00d7d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:11:51Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Computes the fraction of tracks in this album whose alpha value is positive (greater than zero), i.e. counts tracks with alpha > 0 and divides by the total number of tracks, returning an f64 in [0,1].
- found: Guards against empty tracks (returns 0.0), then counts tracks with alpha > 0.0 and divides by total track count — matches prediction exactly.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `alpha_iqr`
- spec 3 · read at `d2181072ed71` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:11:40Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Computes the interquartile range (Q3 - Q1) of per-track alpha values stored on AlbumData, analogous to sibling median methods. Likely sorts a copy of the alpha vector and indexes at approximate 25th/75th percentile positions, returning the difference as f64.
- found: Collects each track's alpha into a vec, sorts it, and returns the difference between the value at the 75th and 25th percentile index (integer division), with a guard returning 0.0 if fewer than 4 tracks.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `usable`
- spec 3 · read at `0936491f6eaf` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:51:47Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Returns true if this AlbumData has enough valid track-level feature data (e.g., a minimum number of tracks with finite/non-NaN values) to be usable for training or scoring the detectors, filtering out degenerate albums.
- found: Returns true if the album has at least 3 tracks; that's the only check.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `train_logreg`
- spec 3 · read at `c19da637a3f7` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:40:48Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Trains a logistic regression classifier via iterative optimization (likely gradient descent) over `samples` (feature slice, boolean label pairs), using L2 regularization strength `lambda` on `nf` weights. Returns a tuple of (weight vector, bias/intercept term) after convergence or a fixed number of iterations.
- found: Standardizes features (z-score using mean/std), does 500 iterations of class-balanced (inverse frequency weighted) gradient descent on L2-regularized logistic loss with fixed learning rate 0.1, then transforms weights/bias back to the original (unstandardized) feature scale before returning.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `score_lr`
- spec 3 · read at `7b1c6e4c2faf` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:48:45Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Computes a logistic regression score: dot product of x and w plus bias b, then applies the sigmoid function (1/(1+exp(-z))) to return a probability between 0 and 1.
- found: Computes only the raw linear logit: bias plus dot product of x and w. No sigmoid is applied here (that must happen at the call site or not at all).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Despite the name "score_lr" (logistic regression), this returns the raw linear combination, not a probability — no sigmoid applied.

### `detector_a_score`
- spec 3 · read at `b99fab4dfd4d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:57:07Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Returns a score for the album based solely on album.deemph_median(), likely returning that value directly as the "score" since detector A is described as a deemph_delta-only detector, with no use of alpha or pe_corr fields.
- found: Returns album.deemph_median() directly as the score.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `detector_b_train` — QUIRKY
- spec 3 · read at `664436fe1305` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:16:44Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Builds album-level feature vectors (from fields like deemph_median, alpha_median, pe_corr_median, frac_pos_alpha, alpha_iqr) for each AlbumData in train_albums, pairs them with ground-truth labels, calls train_logreg to fit a logistic regression, and returns the learned weight vector plus a bias term as (Vec<f64>, f64).
- found: Flattens each album's per-track shape.features vectors (not pooled album stats) into individual training samples labeled with the album's is_pe flag, then calls train_logreg with a fixed regularization of 0.1 to get weights and bias — this is the track-shape stage, not the pooled album stage.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `detector_b_album_score` — QUIRKY
- spec 3 · read at `333291dbbf68` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:56:05Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Builds a feature vector for the album from its pooled statistics (e.g. deemph_median, alpha_median, pe_corr_median, frac_pos_alpha, alpha_iqr) and passes it along with the trained logistic regression weights `w` and bias `b` to score_lr, returning the resulting album-level score representing detector B's two-stage (track shape → pooled → album) prediction.
- found: Scores each track in the album individually via score_lr on the track's shape features and the given weights/bias, sorts the per-track scores, and returns the median (NaN if there are no tracks) — the "pooled → album" step is literally taking the median of per-track logistic scores, not scoring a pooled feature vector.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: The file_doc describes the pipeline as track shape → pooled → album but doesn't say pooling means per-track score-then-median rather than pooling raw features before one logistic-regression call.

### `veto_features` — QUIRKY
- spec 3 · read at `9019ae577c19` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:17:32Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Builds the feature vector fed to the secondary veto logistic-regression model, combining main_score and deemph_score with several AlbumData summary statistics (deemph_median, alpha_median, pe_corr_median, frac_pos_alpha, alpha_iqr) into a single Vec<f64> in a fixed order matching what train_logreg/score_lr expect.
- found: Builds a 6-element feature vector: main_score, deemph_score, alpha_median, frac_pos_alpha, pe_corr_median, and the difference main_score - deemph_score as an explicit agreement feature. I guessed deemph_median and alpha_iqr instead of frac_pos_alpha and the explicit difference term.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `tune_threshold` — QUIRKY
- spec 3 · read at `98bce780850b` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:50:34Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Given (score, label) pairs, filters out the negative-label scores, sorts them, and picks a threshold such that only target_fpr fraction of negatives would score above it (i.e. indexes into the sorted negative scores at the position corresponding to target_fpr), returning that value as the classification threshold to use for the detector.
- found: Actually does a brute-force search over all candidate score values, computing true-positive and false-positive rate at each threshold, and picks the threshold with the highest true-positive count among those whose FPR is within target_fpr — a max-recall-subject-to-FPR-constraint search rather than my simpler percentile-indexing approach.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `collect_files`
- spec 3 · read at `d4bae4680aca` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:03:38Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Reads the directory entries of `dir`, filters to regular files (possibly by an audio extension like .flac/.wav), collects their paths into a Vec<PathBuf>, and likely sorts them for deterministic ordering across test runs.
- found: Recursively walks the directory tree with walkdir, keeps only entries with a .flac extension, and collects their paths into a Vec — no sorting.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `album_name`
- spec 3 · read at `95df5056c135` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:16:54Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Takes the parent directory of the given path and returns its file_name as a String (with some fallback like "unknown" if either is missing), used as a human-readable album identifier for grouping tracks by album in the experiment.
- found: Returns the parent directory's file_name as a String, falling back to \"?\" if parent, file_name, or utf8 conversion fails.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `album_key` — QUIRKY
- spec 3 · read at `2355f94400b4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:31:16Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Normalizes a track/file name string into a canonical album-grouping key by stripping track-number prefixes/suffixes and file extensions and lowercasing, so tracks belonging to the same album consistently map to the same key for grouped cross-validation splits.
- found: Truncates the name at the first '(' character (trimming whitespace), e.g. to strip a parenthetical disambiguator like a year or edition tag, otherwise returns the name unchanged; much simpler than the track-number-stripping normalization I guessed.
- predicted: some · documented: none · derivable: no · legible: full · trap: no

### `veto_experiment`
- spec 3 · read at `30f62c5802d4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:16:07Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Loads the labeled corpus, computes per-track features and aggregates them into AlbumData, then runs grouped (by-album) cross-validation to train and evaluate the four compared systems (A: deemph-only, B: two-stage, C: two-stage+veto, D: stacked meta-model). For each fold it trains detector_b and a veto/meta logistic regression via train_logreg, scores albums, tunes a decision threshold, and prints comparative recall/false-positive metrics across systems to determine whether the veto model reduces false positives without hurting PE recall.
- found: Scores PE and non-PE tracks, groups into albums, runs 3-fold grouped CV training detectors A/B, a veto logreg model, and a 2-expert stack meta-model per fold, evaluates all four systems' OOF recall/FPR at a target FPR, prints per-FP veto detail and matched PE/non-PE pair comparisons, then prints an automated recommendation of which system to ship based on the FP/recall tradeoffs.
- predicted: most · documented: full · derivable: no · legible: most · trap: no

## tests/settings_sentinel.rs

### the file itself
- spec 3 · read at `3a78eebf80bd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:12:09Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: This is a "sentinel" regression-test file whose job is to prevent silent field-dropping in the settings/conversion pipeline: it defines sentinel settings values (deliberately set to non-default values, including DSD-specific and legacy variants) and asserts that every field of the pipeline settings struct survives, field-by-field, as it's translated through ConversionOptions → ConversionItem → PipelineRequest and through a separate legacy-options projection path. It also has meta-tests that mechanically check the test suite's own field inventory/fingerprint stays in sync with the actual settings struct (so a newly added field can't be forgotten), plus tests for known conflicts and rejection of legacy-only items by the normal request builder.
- found: Matches the core prediction: sentinel non-default settings fixtures, field-by-field preservation assertions across ConversionOptions→ConversionItem→PipelineRequest, legacy-projection classification/coverage checks, and meta-tests keeping the field inventory in sync with the settings struct. Missed one nuance: a single all-non-default sentinel is actually impossible due to real field conflicts (e.g. store_source_audio_md5 requiring FLAC+transfer_tags), so the suite documents that as an executable finding and uses a valid pair of sentinels instead — more subtle than my straightforward 'sentinel values are asserted to survive' framing.
- predicted: most · documented: none · derivable: yes · legible: not judged · trap: no

### `legacy_dsd_behavior` — QUIRKY
- spec 3 · read at `cdf86d38010b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:18:15Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Builds a LegacyDsdBehavior value by extracting only the subset of DsdSettings fields relevant to old/legacy DSD conversion behavior (e.g. gain mode, noise shaper, modulator order), ignoring newer tuning params like trellis/sinc settings — used by tests to verify legacy-relevant fields still map to expected behavior categories.
- found: It's a thin test helper that just calls settings.legacy_behavior() and falls back to a specific default LegacyDsdBehavior (Auto lowpass, Disabled gain mode, 0.15 margin, no gain_db) if that returns None, rather than manually deriving fields itself.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `sentinel_dsd_settings`
- spec 3 · read at `0db42561a75e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T20:37:40Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Fixture helper that constructs a DsdSettings value with every field set to a distinctive non-default "sentinel" value, used by other tests to round-trip settings through conversions/pipeline building and confirm no field is silently dropped or reset to its default.
- found: Builds a DsdSettings sentinel fixture by constructing a serde_json object with every field set to a distinctive non-default value (enums, nested TrellisSettings/SincFilterSettings structs, floats) and deserializing it via serde_json::from_value, expecting success.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Uses JSON round-trip via serde rather than a direct struct literal, presumably so the fixture also exercises/validates the deserialization schema.

### `raw_all_non_default_sentinel`
- spec 3 · read at `c710bdcd6bee` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:44:55Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Constructs and returns a PipelineSettings value with every field explicitly set to a non-default value, serving as a fixture "sentinel" used by other tests in this file to verify that conversions/projections (e.g. conversion_item_to_pipeline_request) preserve all settings fields rather than silently resetting some to default.
- found: Builds a fully-populated PipelineSettings literal with every field set to a deliberately non-default value (custom format, unusual sample rate/bit depth, all per-codec settings, dsd/metadata/verification/replay-gain sections) to serve as a sentinel fixture for field-preservation round-trip tests.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `flac_md5_sentinel` — OBSCURE
- spec 3 · read at `21b48a2fa3ca` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:00:23Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Builds and returns a PipelineSettings value that sets the output format to FLAC and flips only the MD5-checksum-related field away from its default, leaving all other fields at default — used as a test fixture for the sentinel-based field-coverage tests seen in the peer list.
- found: Starts from raw_all_non_default_sentinel() (a base with every field pushed away from default) and then overrides target_format to Flac, target_bit_depth to 24-bit PCM, and metadata.transfer_tags to true — producing a valid combination usable for FLAC where those fields would otherwise conflict with FLAC's constraints.
- predicted: none · documented: some · derivable: no · legible: full · trap: no
- note: The name suggests it's about FLAC's MD5 field, but the body never touches an MD5 field directly — it's really about which non-default field combinations are valid for FLAC output; the doc comment ('Valid FLAC sentinel for fields whose non-default values require FLAC output') is the only clue tying the name to intent.

### `custom_format_sentinel` — QUIRKY
- spec 3 · read at `92f27280b526` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:18:16Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Builds a PipelineSettings test fixture for WAV output with Float32 sample format, while also setting FLAC-only fields (like MD5 checksum verification or native FLAC verify flags) to values that conflict with WAV's rules — used to test that format-specific validation correctly rejects or ignores these settings when the container format isn't FLAC.
- found: Starts from raw_all_non_default_sentinel() (all fields non-default) then overrides target_format to AudioFormat::Custom (extension "sent"), and disables flac.verify and metadata.store_source_audio_md5 since those conflict with a non-FLAC custom format.
- predicted: some · documented: most · derivable: no · legible: full · trap: no

### `valid_sentinels`
- spec 3 · read at `5341aa73c3f9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:51:52Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Returns a fixed array of two PipelineSettings values, each constructed via one of the sentinel helper functions in this file (e.g. flac_md5_sentinel and custom_format_sentinel), representing valid non-conflicting settings combinations that other tests iterate over to check field preservation through conversion/pipeline transforms.
- found: Exactly as predicted: array literal of flac_md5_sentinel() and custom_format_sentinel().
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `item_with_settings`
- spec 3 · read at `1a4aebd4d1d2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:53:51Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A test helper that constructs a ConversionItem with the given PipelineSettings plugged in and all other fields (source path, id/queue position, format, etc.) filled with arbitrary-but-valid placeholder/default test values, used by the sentinel tests to build items focused on settings variation.
- found: Builds default ConversionOptions, sets output_format via queue_format_for_settings(&settings) and pipeline_settings to Some(settings), then constructs a ConversionItem with a fixed dummy FLAC input path and Audio(Flac) file format.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `queue_format_for_settings`
- spec 3 · read at `1e086e34dcc7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:44:52Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A test helper that maps a PipelineSettings' format field to the corresponding QueueAudioFormat variant, likely via a match/From-like conversion, used to construct queue items with settings whose format field is consistent with the audio format for sentinel/round-trip tests.
- found: Matches settings.target_format across all AudioFormat variants to the corresponding QueueAudioFormat variant one-to-one, except Custom{..} which falls back to QueueAudioFormat::Flac.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `assert_settings_eq`
- spec 3 · read at `449f3c145976` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:23:46Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A long sequence of per-field assert_eq! calls comparing actual and expected PipelineSettings field by field, each with a descriptive failure message naming the field, so that a test failure pinpoints exactly which settings field diverged rather than dumping the whole struct diff.
- found: A long flat sequence of per-field assert_eq! calls covering every PipelineSettings field (target format/rate/depth, per-codec settings, resampler settings, DSD/PCM-to-DSD settings including trellis/sinc, metadata, verification, replay gain), each tagged with the field name string for a precise failure message.
- predicted: full · documented: none · derivable: yes · legible: most · trap: no

### `field_differs_from_default`
- spec 3 · read at `c6ea108b02d3` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:28:58Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A match statement keyed on the path string, one arm per PipelineSettings field (including nested ones like dsd.* or metadata.*), each arm comparing settings.<field> != default.<field> and returning the bool; likely panics or returns false/unreachable for an unknown path, used by the sentinel tests to check each named field actually differs from its default.
- found: Large match on the dotted path string, one arm per PipelineSettings field (including nested dsd/sox/ssrc/metadata/replay_gain fields, some routed through legacy_dsd_behavior for legacy-projected fields), comparing settings vs default and returning whether they differ; panics on an unrecognized path.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `known_conflict_test`
- spec 3 · read at `aa6b3d0b7d4d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:07:46Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the given test name is in a hardcoded list of test names known to represent "conflicting" sentinel scenarios (e.g. tests that are expected to fail or flag due to setting conflicts), returning true if `name` matches one of those known entries.
- found: Matches name against a fixed set of three named constants representing known conflicting settings combinations (md5-requires-flac-output, md5-requires-metadata-transfer-tags, flac-verify-requires-flac-output).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `sentinel_suite_inventory_matches_fingerprint_field_list`
- spec 3 · read at `22639c3d9ba9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:58:09Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Asserts that a hardcoded/generated inventory of field names covered by the sentinel test suite exactly matches the canonical list of fields used for fingerprinting PipelineSettings, acting as a mechanical safety net so a newly added settings field without sentinel coverage fails this test.
- found: Sorts and compares SENTINEL_FIELD_INVENTORY paths against SETTINGS_FINGERPRINT_FIELD_PATHS for equality (and no duplicates), plus a count check against SETTINGS_FINGERPRINT_FIELD_COUNT — confirming my general idea but with more specific mechanics (dedup check, count check) than I predicted.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `sentinel_suite_inventory_classification_is_mechanically_checked`
- spec 3 · read at `5d71266975d1` · commit `1681528` · read by claude-sonnet-5 · when 2026-08-19T08:34:11Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A test that iterates over the suite's sentinel test cases (e.g. valid_sentinels vs known-conflict cases) and mechanically verifies their classification — asserting, via helpers like field_differs_from_default or assert_settings_eq, that entries labeled 'valid' actually cover distinct non-default fields and that 'known conflict' entries are truly conflicting — rather than trusting hand-maintained comments/labels, so the suite can't silently drift out of sync with the real PipelineSettings field list.
- found: Walks a static SENTINEL_FIELD_INVENTORY table, and for each field entry asserts: it's marked fingerprint-covered; whether it differs from default in the 'raw all non-default' sentinel matches the entry's recorded raw_drift_covered flag; whether any 'valid' sentinel covers it matches the recorded valid_propagation_covered flag; each named conflict test it references is a real known conflict test; and if it isn't covered by any valid sentinel, it must have at least one named conflict test explaining why.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: The per-field inventory table (SENTINEL_FIELD_INVENTORY) with its four independently-checked classification flags is the real structure here — not visible from the name/peers, which only hint at 'mechanical checking' in the abstract.

### `raw_single_sentinel_sets_every_field_away_from_default`
- spec 3 · read at `6449e0008880` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:34:07Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Builds a single "raw sentinel" settings/options value meant to set every field to a non-default value, then iterates the fields (likely via field_differs_from_default helper) asserting each one differs from Default::default(), to guard against a sentinel fixture silently leaving some field at its default and thus not exercising it in other tests.
- found: Explicitly asserts, field by field via an assert_covered_by_non_default! macro, that every field of raw_all_non_default_sentinel() differs from PipelineSettings::default(), including nested config structs (flac, mp3, aac, opus, wavpack, ssrc, sox/soxr resamplers, dsd, metadata, verification, replay_gain), plus manual asserts for legacy DSD fields, and finally asserts the sentinel fails validate() (since it's intentionally inconsistent).
- predicted: most · documented: none · derivable: yes · legible: most · trap: no
- note: Not a generic loop — it's an explicit enumerated macro call per field, so adding a new settings field requires manually adding a line here or this coverage check silently misses it.

### `amended_contract_valid_sentinel_set_covers_every_pipeline_settings_field`
- spec 3 · read at `19c7b8197345` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:02:39Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Builds the "amended" version of the valid-sentinel-set contract (likely a corrected/expanded variant of an earlier test) and checks that, for every field in PipelineSettings, some sentinel in the set changes that field away from its default (using field_differs_from_default), failing loudly if any settings field added later is not exercised by any sentinel — guarding against silent field additions that a change-detection contract doesn't cover.
- found: Constructs default/flac/custom sentinel settings instances, validates them, then asserts field-by-field (via an assert_covered_by_non_default! macro, one line per PipelineSettings field, ~65 fields) that at least one sentinel differs from default for that field, with manual boolean asserts for the legacy DSD fields. My prediction correctly identified the coverage-contract purpose and the per-field non-default check but underestimated the sheer exhaustive enumeration and the two-sentinel (flac+custom) design plus the legacy-DSD special-casing.
- predicted: most · documented: none · derivable: yes · legible: most · trap: no

### `single_valid_all_non_default_sentinel_conflict_is_executably_documented` — OBSCURE
- spec 3 · read at `4150897e3ad7` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:07:11Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: This test asserts that among the "valid sentinels" (values where all pipeline settings fields are pushed away from their defaults), exactly one field triggers a known conflict case, and that this conflict is captured by an executable check (via known_conflict_test) rather than just a comment - i.e. it finds/counts the single conflicting field and verifies it matches the documented exception.
- found: Directly constructs three specific sentinel settings combos known to conflict (MD5 requires FLAC output, MD5 requires metadata transfer_tags, FLAC verify requires FLAC output) and asserts validate() rejects each, executably documenting these three named conflict constants rather than deriving/counting conflicts from valid_sentinels().
- predicted: none · documented: none · derivable: yes · legible: full · trap: no

### `conversion_options_to_conversion_item_preserves_settings_field_by_field`
- spec 3 · read at `a0261d4500d1` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:14:32Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Builds conversion options/settings with every field set to a non-default sentinel value, converts them into a ConversionItem, and calls assert_settings_eq to verify every individual settings field survived the conversion unchanged (part of a suite guarding against silently-dropped fields).
- found: Iterates over a set of valid sentinel settings (each presumably a distinct non-default field combination), builds a ConversionItem for each via item_with_settings, and asserts the item's pipeline_settings equals the original sentinel field-by-field via assert_settings_eq.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `conversion_item_to_pipeline_request_preserves_settings_field_by_field`
- spec 3 · read at `da3cd1245fad` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:17:59Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Test: constructs a ConversionItem whose settings have every field set away from its default (a sentinel), converts it into a pipeline request via the production conversion path, then calls assert_settings_eq to check field-by-field that no setting was dropped, defaulted, or altered in the conversion — likely a short 4-5 line body wrapping a sentinel builder and the assertion helper.
- found: Iterates over a set of valid_sentinels (not just one), builds a ConversionItem for each, runs it through build_pipeline_request, and asserts field equality against the sentinel — testing every valid sentinel combination rather than a single fixed one.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `prebuilt_pipeline_request_preserves_settings_field_by_field` — QUIRKY
- spec 3 · read at `276e68f7e8cd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:37:27Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Builds a sentinel PipelineSettings with every field pushed away from its default, constructs a PipelineRequest directly from it (the "prebuilt" path, not via the ConversionOptions/ConversionItem conversion chain), and calls assert_settings_eq to verify every field on the resulting request's settings still matches the sentinel — proving the prebuilt-request construction path drops or overwrites nothing.
- found: For every sentinel in valid_sentinels(), builds an item with those settings, builds a pipeline request from it, then overwrites that request's settings with the sentinel and stashes it back onto item.pipeline_request (simulating an already-prebuilt request attached to the item). It then calls build_pipeline_request again and asserts the resulting settings still equal the sentinel field-by-field — verifying that when an item already carries a prebuilt request, build_pipeline_request preserves/respects its settings rather than deriving/overwriting them from the item.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no

### `normal_request_builder_rejects_legacy_only_items` — QUIRKY
- spec 3 · read at `c7b6d58d64e9` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:31:46Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: This test builds a legacy-only queue item (likely via the legacy_item helper) and asserts that the normal (non-legacy) request builder returns an Err or otherwise rejects it, verifying that legacy-format items cannot be processed through the standard pipeline request construction path.
- found: Constructs a plain ConversionItem for a FLAC file with default ConversionOptions (not via a dedicated 'legacy_item' helper) and asserts build_pipeline_request returns Err, presumably because default options aren't a valid/complete settings set for the normal builder.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no
- note: Function name implies a 'legacy-only' concept but the body just uses ConversionOptions::default() — the connection to 'legacy' isn't visible without reading build_pipeline_request's validation logic.

### `legacy_projection_inventory_lists_every_pipeline_settings_field`
- spec 3 · read at `374d06c40383` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:42:42Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: A test asserting that a hardcoded/maintained inventory of field names used for "legacy projection" coverage checks stays in sync with the actual fields of the PipelineSettings struct — comparing the inventory list against a fingerprint or reflection-based field list of the struct and failing if they diverge (e.g., a new field added to the struct but not reflected in the inventory).
- found: Sorts both LEGACY_FIELD_INVENTORY's field-path names and SETTINGS_FINGERPRINT_FIELD_PATHS, then asserts equality — confirming the legacy-projection inventory covers exactly the same set of settings fields as the fingerprint list, no more, no less.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `legacy_item`
- spec 3 · read at `1fdd20da88a8` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:07:50Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Test helper that wraps the given ConversionOptions into a ConversionItem, filling in placeholder/default values for the remaining fields (like a source path or id) needed to construct a valid item, so tests can focus on varying just the settings.
- found: Constructs a ConversionItem via ConversionItem::new using a fixed placeholder path (/tmp/.../legacy.flac), a fixed FLAC FileFormat, and the passed-in options — exactly the placeholder-wrapping helper predicted.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `rich_legacy_flac_options`
- spec 3 · read at `f1fdb84e31bf` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:03:25Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A fixture-builder helper (not itself a test) that constructs a ConversionOptions for FLAC format with a rich set of legacy-only option fields set to non-default values, used by other tests in this file to verify that legacy field projection/preservation covers every field.
- found: Builds a ConversionOptions for FLAC with many non-default fields set (compression level 8, replaygain both, resample quality, nyquist transition, dither type, target rate/depth, reencode_flac, sox backend, ssrc insane mode) — a fixture for legacy field coverage tests.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `legacy_projection`
- spec 3 · read at `5de3cbbfd9b9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:39:48Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A 3-line test helper that calls into the production code path converting a ConversionItem to a PipelineRequest via the "legacy" conversion route, likely just delegating to an existing conversion function (e.g. item.to_pipeline_request() or similar) so other sentinel tests can compare its output field-by-field against expectations.
- found: Delegates directly to tonepoet::convert::pipeline::build_pipeline_request_from_legacy_options(item).unwrap(), a thin wrapper around the actual production legacy-conversion function.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `legacy_options_for_quality` — QUIRKY
- spec 3 · read at `ce102578a1bd` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:50:49Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: A test helper that constructs a ConversionOptions (the legacy options struct) with the given output_format and quality plugged in, and all other fields set to their defaults, for use in round-trip/projection comparison tests.
- found: Starts from rich_legacy_flac_options() (a fully non-default sentinel baseline, per the peer raw_single_sentinel_sets_every_field_away_from_default) rather than plain defaults, then overrides just output_format and quality.
- predicted: some · documented: none · derivable: no · legible: full · trap: no
- note: The base is a deliberately all-non-default sentinel fixture, not Default::default() — matters for tests relying on every other field staying away-from-default.

### `assert_legacy_coverage`
- spec 3 · read at `b2732e10ceba` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:08:33Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Compares the given `covered` list of (field_name, LegacyProjectionStatus) pairs against the full canonical inventory of pipeline settings fields, asserting every field appears exactly once with no duplicates and none missing — so a test author is forced to explicitly account for every field's legacy-projection status, and adding a new settings field without updating the test list causes a failure.
- found: Sorts both the caller-supplied `covered` list and the canonical LEGACY_FIELD_INVENTORY, then asserts they're equal — so any drift (missing, extra, or status-mismatched field) between what a test explicitly claims to cover and the real inventory fails with a clear message.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `explicit_legacy_projection_has_behavioral_assertion_for_every_field` — QUIRKY
- spec 3 · read at `6e6720672f4d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:13:06Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Iterates the inventory of PipelineSettings field names and checks that each one has an explicit, executable behavioral assertion in the legacy projection test suite, panicking/asserting with the specific missing field name(s) if any field lacks coverage — guarding against silently-unverified settings fields as new ones are added.
- found: Not a loop over an inventory — it builds several legacy-projected settings snapshots (flac/mp3/aac/opus/wavpack) and then hand-writes ~65 explicit assert_legacy_value!/assert_legacy_unrepresentable! macro calls, one per PipelineSettings field, each asserting both the concrete expected value AND a LegacyProjectionStatus classification (Translated/Defaulted/Derived) while accumulating into `covered`; assert_legacy_coverage(&covered) at the end presumably cross-checks that list against the full field inventory to catch anything left out.
- predicted: some · documented: none · derivable: yes · legible: most · trap: no
- note: The real mechanism is the closing assert_legacy_coverage(&covered) call comparing the accumulated field list against the mechanically-generated inventory (seen in a peer test name) — that cross-check isn't visible in this function alone.

### `native_v2_dsd_settings_use_a_separate_complete_snapshot_inventory`
- spec 3 · read at `27ea01491e45` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:49:21Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A sentinel/meta test asserting that there's a distinct, complete field-name inventory list specifically for native v2 DSD settings — separate from the general pipeline-settings field inventory — and that it covers every field of the relevant struct, so that a mechanical coverage check doesn't silently miss new DSD-specific fields.
- found: Checks a specific constant SETTINGS_SNAPSHOT_V2_DSD_FIELD_PATHS array has no duplicates, that its length matches a separately declared FIELD_COUNT constant, and spot-checks a few specific expected field-path strings are present — a narrower mechanical check than a general "completeness" verification against a struct's actual fields.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The completeness/coverage claim in the test name is not actually verified here against the real struct fields — it only cross-checks two hand-maintained constants (PATHS vs COUNT) plus a few spot-checked strings, so it can't catch a field missing from both.

## tests/settings_static_audit.rs

### the file itself
- spec 3 · read at `79657fe20995` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T07:15:49Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: A lint-style architectural conformance test with no module doc, using `syn`'s Visit trait to statically walk the actual Rust source tree (not runtime behavior). It defines two audits, PipelineSettingsDefaultAudit and PlanRequestLiteralAudit, that recursively parse .rs files under src/ and walk each file's AST to catch two anti-patterns: (1) production code silently constructing PipelineSettings::default() instead of explicitly threading real settings through, and (2) PlanRequest/PipelineRequest struct literals that don't preserve/carry the typed settings field. It supports inline "allowance" comments near a line to suppress specific flagged violations, skips #[cfg(test)] code, and the two #[test] entry points (production_code_does_not_construct_silent_pipeline_settings_defaults, production_plan_request_literals_use_typed_pipeline_request_settings) fail if any unallowed violation is found.
- found: A no-doc static architectural conformance test using syn's Visit trait to parse all .rs files under src/ (and tonepoet-pipeline/src/) and run two AST audits: PipelineSettingsDefaultAudit flags any silent PipelineSettings::default()/Default::default() construction (as a call, local init, assignment, struct-update-rest, or settings field), and PlanRequestLiteralAudit flags any PlanRequest struct literal whose `settings` field isn't derived (via direct field read or .clone()/.to_owned()) from a typed PipelineRequest parameter in scope. Both support a `settings-sentinel-allow` comment within 3 lines above the violation to suppress it, and skip #[cfg(test)] modules/functions; two #[test] entry points assert zero violations (the second also asserts at least one literal was actually found, guarding against the audit silently checking nothing).
- predicted: full · documented: none · derivable: yes · legible: most · trap: no
- note: None.

### `production_code_does_not_construct_silent_pipeline_settings_defaults`
- spec 3 · read at `b0a09e691227` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:50:14Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Walks all production (non-test) Rust source files via rust_files_under/visit_dir, parses each with parse_rust_file, runs the PipelineSettingsDefaultAudit syn visitor over them to detect any PipelineSettings::default()-style construction, and asserts the audit's report() is empty — enforcing that pipeline settings must always be explicitly constructed rather than silently defaulted.
- found: Walks src/ and tonepoet-pipeline/src/ for Rust files, runs audit_pipeline_settings_defaults on each collecting violations into a BTreeSet, and asserts the set is empty with a formatted violation list on failure.
- predicted: most · documented: none · derivable: no · legible: full · trap: no
- note: Confirmed the higher-level structure (walk, audit, assert-empty) but the actual visitor call was via a helper function audit_pipeline_settings_defaults rather than direct visitor invocation with parse_rust_file inline as I guessed.

### `production_plan_request_literals_use_typed_pipeline_request_settings`
- spec 3 · read at `b3c6109d6a2d` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:11:59Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Walks production source files under src/, parses each with syn via parse_rust_file, runs the PlanRequestLiteralAudit visitor to detect plan-request struct literals constructed without going through typed PipelineRequestSettings, and asserts audit.report() is empty, failing the test with the violation list if any production code bypasses the typed settings path.
- found: Walks src/ rust files, runs audit_plan_request_literals per file accumulating a violations set and a literal_count; asserts literal_count > 0 (guarding against the audit silently matching nothing) and asserts violations is empty, printing offending literals if not.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `rust_files_under`
- spec 3 · read at `9517e75336c2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:48:53Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Recursively walks the directory tree starting at root (likely delegating to a visit_dir helper), collecting the paths of all files ending in .rs, skipping directories like target/ or .git, and returns them as a Vec<PathBuf> for source-level static-audit tests to parse with syn.
- found: Thin wrapper: delegates the actual recursive walk to visit_dir(root, &mut out), then sorts the collected paths before returning them, presumably for deterministic test iteration order.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `visit_dir`
- spec 3 · read at `5c47c5647be0` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:05:35Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Recursively walks the directory tree rooted at path, reading directory entries; for subdirectories it recurses, and for files ending in .rs it pushes the path into out, building up the list of Rust source files for the static audit to later parse.
- found: Recursively walks a directory, silently returning if it can't be read, recursing into subdirectories and collecting .rs file paths into out.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `parse_rust_file`
- spec 3 · read at `45b7fcc0b32e` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:18:16Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Reads the file at `path` to a String and parses it with syn::parse_file into a syn::File AST, returning both the raw source text and the parsed AST as a tuple; panics/unwraps on read or parse failure since this is a test-only helper.
- found: Reads file to string and syn::parse_file's it, panicking with a descriptive message including the path on either failure, returning (source, parsed) tuple.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `audit_pipeline_settings_defaults`
- spec 3 · read at `39267864d329` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:45:38Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Parses the Rust source file at path using parse_rust_file, runs the PipelineSettingsDefaultAudit syn visitor over the resulting AST to detect silent/default construction of pipeline settings, and inserts whatever violation messages the visitor's report() produces into the violations set.
- found: Parses the file, collects local type names/aliases resolving to "PipelineSettings" via collect_type_names, constructs a PipelineSettingsDefaultAudit visitor seeded with those names and empty tracking state, and runs it over the parsed file to populate violations.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `audit_plan_request_literals`
- spec 3 · read at `8df740fb53ce` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:34:58Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Parses the Rust file at path into a syn AST, runs the PlanRequestLiteralAudit visitor over it to detect places that construct PlanRequest-like struct literals directly instead of via a typed constructor, inserts any violation descriptions (e.g. file:line) into the violations set, and returns a count (e.g. number of items visited or violations found in this file).
- found: Parses the file, builds a PlanRequestLiteralAudit visitor seeded with the local type-alias names resolving to PlanRequest/PipelineRequest, walks the file collecting violations into the passed-in set, and returns the visitor's literal_count field.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `collect_type_names`
- spec 3 · read at `cf6e4c3bcd7f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:12:28Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Walks the parsed file's `use` items looking for imports of `canonical` (possibly renamed via `as`), and collects the set of local names (aliases plus the bare canonical name) that this file could use to refer to that type — used so the audit can recognize the type under any import alias, not just its canonical name.
- found: Starts with the canonical name, adds names from `use` import aliases, then runs a fixed-point loop over `type X = ...;` item aliases, adding any alias whose definition references an already-known name — so it transitively picks up type aliases of aliases, not just direct use-renames.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `collect_use_aliases`
- spec 3 · read at `5f1289435688` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:10:58Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Recursively walks a syn `UseTree` (matching on Path/Name/Rename/Glob/Group variants), and whenever it finds a leaf item whose path matches `canonical` (the fully-qualified type name being tracked, e.g. `PipelineSettings`), inserts the locally-visible name into `names` — using the rename target if the use has an `as` alias, otherwise the original name. This lets the audit recognize the type under any import alias used in a file.
- found: Recursively walks the UseTree; matches Name/Rename leaves whose final ident equals `canonical` (a bare identifier, not a full path — path prefix segments are ignored via the Path variant just recursing), inserting either the plain name or the rename target into `names`; Group items are recursed into; Glob is ignored. Confirms my general recursive-alias-collection idea but I overstated it as matching a full qualified path rather than just the trailing ident.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `report`
- spec 3 · read at `ef8dc7e78291` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:51:03Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Records a lint violation found during the AST walk: formats a message using the current file path and the span's line/column (via span.start()) combined with `detail`, and pushes it into a Vec of violation strings stored on self (or self.errors) so the test can later assert the collected list is empty.
- found: Checks if the violation's line has a nearby suppression/allowance comment via has_allowance_near_line and skips reporting if so; otherwise formats "path:line: detail" and inserts it into a self.violations set.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `visit_item_mod`
- spec 3 · read at `fe4ec31a46a9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:03:28Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: This is a syn::Visit implementation for a static-analysis lint (auditing pipeline settings defaults). I expect visit_item_mod checks if the module is a #[cfg(test)] test module and skips descending into it (to avoid false positives from test code), otherwise calls the default visit::visit_item_mod to continue the AST walk.
- found: Skips descending into modules marked #[cfg(test)] via has_cfg_test, otherwise continues the default AST visitation into the module.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `visit_item_fn` — OBSCURE
- spec 3 · read at `a448443827b0` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:01:48Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Part of a syn::visit::Visit implementation used by this static audit test: it records the current function's name (pushing it onto some context/stack for later diagnostic messages), then calls syn::visit::visit_item_fn(self, node) to continue walking into the function body so nested expressions get visited by the other visit_* methods, then likely pops the context afterward.
- found: Skips functions marked #[cfg(test)] entirely; otherwise checks if the function's return type mentions one of the tracked pipeline settings type names, and if so increments a pipeline_return_depth counter around the recursive visit (so nested visitors know they're inside a function that returns PipelineSettings), then decrements after; always recurses via syn::visit::visit_item_fn.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: The depth counter, not a name stack, is how downstream visit_* methods know they're inside a PipelineSettings-returning function — that context signal is easy to miss from the signature alone.

### `visit_item_type` — QUIRKY
- spec 3 · read at `4658c6403a76` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:05:53Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A syn::visit::Visit trait method for visiting Rust `type` alias items during static AST analysis. It likely records the type alias name/target (perhaps into a type-alias map for resolving struct field types later) and then delegates to visit::visit_item_type to continue the traversal into nested items.
- found: If the aliased type references any name already in self.pipeline_names, adds this alias's own name to pipeline_names too (transitive closure over type aliases), then continues the visit via syn::visit::visit_item_type.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `visit_local`
- spec 3 · read at `f4c8eb32f273` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:10:46Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Inspects a let-binding's pattern/type annotation or initializer expression for a match against the tracked settings type name (e.g. to catch local variables typed as the pipeline settings struct being assigned outside the canonical default constructor), records any match as an audit finding, and then calls syn::visit::visit_local(self, node) to continue walking into nested expressions.
- found: Checks if the let-binding has a type-annotated pattern whose type contains one of the tracked pipeline settings type names; if so, records the local variable name in typed_pipeline_locals, and if the initializer is specifically a Default::default() call, reports it as a finding (flagging that PipelineSettings was default-constructed rather than via the canonical constructor); then continues the visit recursion.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: Correctly predicted the shape but missed that it also tracks the local's name for later cross-referencing (typed_pipeline_locals) separate from the immediate default-call report.

### `visit_expr_call`
- spec 3 · read at `931dfd92d94d` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:28:21Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Part of a syn-based static audit visitor. Checks whether the call expression's callee matches a specific target function path (e.g. a pipeline settings constructor or Default::default equivalent), and if so records something into the audit's report/state (perhaps flags a violation or logs the call site), then calls syn::visit::visit_expr_call to continue recursing into the call's arguments so nested calls are still checked.
- found: Flags two specific forbidden patterns via self.report: a direct PipelineSettings::default() call, or (when inside a function that returns PipelineSettings, tracked by pipeline_return_depth) any trait Default::default() call — presumably to force settings to be constructed explicitly rather than silently defaulted — then recurses via syn::visit::visit_expr_call.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `visit_expr_assign` — QUIRKY
- spec 3 · read at `933c029b5e60` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:38:35Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This is part of a syn::visit::Visit implementation (PipelineSettingsDefaultAudit) that walks test/source ASTs looking for places pipeline settings defaults are set. visit_expr_assign likely inspects the assignment's left-hand side to see if it's a field assignment to a tracked settings field, records something (e.g. pushes a violation or marks a field as covered) if so, and then calls syn::visit::visit_expr_assign (or similar) to continue traversing into sub-expressions.
- found: Checks whether the RHS of an assignment is a Default::default() call and the LHS is a local variable known to be typed as PipelineSettings; if so reports a violation, then continues visiting via syn::visit::visit_expr_assign.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `visit_expr_struct`
- spec 3 · read at `1c30a941201a` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:15:58Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This is a syn::visit::Visit impl method on PipelineSettingsDefaultAudit that inspects struct-literal expressions (ExprStruct) to find places where a pipeline "Settings" struct is being constructed. It likely checks the struct's path/type name against a known settings type, and if it matches, verifies each field initializer against expected default values (recording a violation/error if a field diverges from the audited default), then calls syn::visit::visit_expr_struct to recurse into nested expressions.
- found: Checks struct-literal expressions matching known pipeline settings type names for two anti-patterns: (1) a `..Default::default()` struct-update rest that calls Default::default() on a pipeline type, and (2) any field literally named `settings` whose value is a Default::default() call — reporting both as violations, then recursing via syn::visit::visit_expr_struct.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `visit_macro` — QUIRKY
- spec 3 · read at `b8498f04d02f` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:23:18Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Since syn's default visitor doesn't look inside macro invocations, this attempts to parse the macro's token stream as an expression (or list of expressions), then recursively visits it with self so struct literals/calls hidden inside macros like assert_eq!/vec! aren't silently skipped by the audit.
- found: Stringifies the macro's token stream and does a crude substring check for both \"PipelineSettings\" and \"default\"; if both appear, reports a finding at that span. Then continues the default recursive visit into the macro.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `report` #2
- spec 3 · read at `ef8dc7e78291` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:56:08Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Records a violation found during the syn-based AST audit: formats the span (likely via its start line/column) together with the detail message into a diagnostic string and pushes it onto a Vec of collected violations/errors stored on self, to be surfaced later when the audit test asserts no violations were found.
- found: Computes the span's start line, skips reporting if there's a suppressing allowance comment near that line (has_allowance_near_line), and otherwise inserts a "path:line: detail" formatted string into a violations set.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `current_pipeline_request_params`
- spec 3 · read at `2a076fa5c44d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:40:14Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Returns the set of variable/parameter names currently in scope (as tracked by this AST-walking audit) that refer to a pipeline request value, likely by reading from a stack/vec field on self and flattening it into a HashSet<String>. Used elsewhere to check whether a struct literal expression references one of these params to verify settings are preserved.
- found: Reads the top of a function_contexts stack and clones the pipeline_request_params set for the innermost function currently being visited, defaulting to empty if no context is active.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `visit_item_mod` #2
- spec 3 · read at `fe4ec31a46a9` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:08:42Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Skips recursing into modules annotated #[cfg(test)] (to avoid flagging test fixture code), and otherwise calls syn::visit::visit_item_mod(self, node) to continue walking the module's inner items for PlanRequest literal audit.
- found: Returns early without recursing if the module has a #[cfg(test)] attribute; otherwise delegates to syn::visit::visit_item_mod to continue the AST walk.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `visit_item_fn` #2
- spec 3 · read at `3fd90acd0ca0` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T05:54:01Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Inspects the function's signature to detect if it has a parameter of the pipeline-request/settings type, records that parameter's identity for use by current_pipeline_request_params, calls the default visit::visit_item_fn to continue traversing the body (so visit_expr_struct can check literals), and then clears/restores that state afterward.
- found: Skips #[cfg(test)] functions, otherwise builds a FunctionContext recording which parameter names are typed as pipeline-request types, pushes it onto a context stack, recurses into the function body via the default visitor, then pops the context.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `visit_expr_struct` #2
- spec 3 · read at `ad967878e9ce` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:00:02Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Part of a syn-based static audit test that walks the source AST looking for struct-literal construction of some "plan request" type; when it finds one, it checks (using helpers like expr_carries_pipeline_request_settings / expr_is_default_call) whether the settings field was properly propagated from the pipeline request params rather than defaulted or omitted, recording a violation via self.report if not, then recurses into child nodes via visit::visit_expr_struct.
- found: Checks if the struct literal's path matches known PlanRequest type names; if so increments a literal_count, fetches the current pipeline request params in scope, and verifies via plan_request_literal_preserves_settings that the settings field is set directly from a typed PipelineRequest param, reporting a violation with a specific suggested fix message if not. Always recurses via syn::visit::visit_expr_struct.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `visit_macro` #2 — OBSCURE
- spec 3 · read at `14da7530e05b` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:18:23Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: This is a syn::Visit trait override for the PlanRequestLiteralAudit AST walker. It likely intentionally does nothing (a no-op stub) to prevent the audit from descending into macro invocations, since macro bodies aren't reliably parseable as normal Rust expressions and could otherwise produce false positives/negatives when checking for pipeline request literal settings.
- found: Stringifies the macro's token stream and reports a finding if it textually mentions "PlanRequest" (catching hidden construction inside macros), then continues the default recursive visit.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no

### `plan_request_literal_preserves_settings`
- spec 3 · read at `9f9816760a7e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:06:33Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Part of a static-analysis test harness (using syn AST visitors) that audits whether struct literal constructions of a "plan request" type preserve certain pipeline settings fields rather than silently dropping them. It likely inspects the fields of an ExprStruct and checks whether each field that should carry pipeline settings either uses a ..base_expr spread or explicitly reads from one of the pipeline_request_params, returning true if the settings are preserved and false if the literal appears to drop them.
- found: Checks whether the struct literal has a field named "settings" whose expression carries the pipeline request settings (delegates to expr_carries_pipeline_request_settings), returning true if such a field exists.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `expr_carries_pipeline_request_settings`
- spec 3 · read at `2cd41c291810` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-19T20:33:33Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Strips syntactic wrappers (references, parens, clones) via strip_wrappers, then checks whether the resulting expression either reads a .settings field off one of the pipeline_request_params (via expr_field_reads_pipeline_request_settings) or refers to a local variable known to be typed as the pipeline settings type (expr_is_typed_pipeline_local), returning true if either holds - i.e. the expression genuinely threads through request-derived settings rather than being a fresh Default::default().
- found: Strips wrappers, then recurses through .clone()/.to_owned() method calls on the receiver, and for a bare field-access expression delegates to expr_field_reads_pipeline_request_settings; everything else is false. No typed-local-variable branch exists.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The peer expr_is_typed_pipeline_local exists in the file but isn't used by this function - it's likely called from a different check, which could mislead someone assuming this function covers all 'carries settings' cases.

### `expr_field_reads_pipeline_request_settings`
- spec 3 · read at `691a15fdc1dc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T04:44:07Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Checks whether field.member is named "settings" and the field's base expression is (after stripping wrappers) a reference to one of the pipeline_request_params, using helper functions member_is_named and expr_is_pipeline_request_param_base, returning true if this ExprField represents reading `.settings` off a known PipelineRequest-typed parameter.
- found: Returns true if the field member is named "settings" and the base expr is a pipeline-request param, via member_is_named and expr_is_pipeline_request_param_base.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `expr_is_pipeline_request_param_base`
- spec 3 · read at `35161ddffbd0` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:55:26Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Strips reference/deref/paren wrappers from the expression, then checks if the resulting expr is a simple path (identifier) whose name matches one of the strings in pipeline_request_params. Used to detect struct-update-syntax bases like `..request` where `request` is a known pipeline request parameter, so the audit can treat all its fields as preserved.
- found: Recursively strips wrapper exprs, then checks if the base expression is a single-segment path whose identifier is in the pipeline_request_params set; recurses through unary exprs (e.g. deref) separately from strip_wrappers.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `strip_wrappers`
- spec 3 · read at `6b2624e252c2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:14:11Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Recursively unwraps syntactic wrapper nodes around an expression — like Expr::Paren, Expr::Group, Expr::Reference (&expr), and maybe Expr::Try or Expr::Cast — returning the innermost "real" expression so the rest of the static audit can pattern-match on the actual call/struct/field access without being confused by parens or reference operators.
- found: Recursively strips Paren, Reference, and Group wrapper expressions to expose the underlying expression, exactly as predicted (I guessed Try/Cast might also be included but they weren't).
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `member_is_named`
- spec 3 · read at `3c5df859a099` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:51:59Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Matches on syn::Member: if it's the Named variant, compares the ident's string representation to `expected` and returns true on equality; if Unnamed, returns false.
- found: Exactly as predicted: matches!(member, Member::Named(ident) if ident == expected).
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `expr_is_typed_pipeline_local`
- spec 3 · read at `9a9f77379c30` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:33:18Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Strips reference/paren wrappers from expr via strip_wrappers, then checks whether the resulting expression is a simple Expr::Path with a single identifier segment, and if so returns whether that identifier's string name is contained in typed_pipeline_locals; otherwise returns false.
- found: Checks whether expr is a single-segment Expr::Path and, if so, whether that identifier's name is in typed_pipeline_locals; no wrapper-stripping is done, unlike I predicted.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `expr_is_default_call`
- spec 3 · read at `8cd502da65a5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:04:35Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Matches on `expr` being an Expr::Call, then delegates to checking the call's function path via helpers like expr_path_is_trait_default_call (matches Default::default()) or expr_path_is_named_default_call (matches SomeType::default() where SomeType is in pipeline_names), returning true if either matches — a static check for whether an expression is a .default()-style construction.
- found: Matches Expr::Call and returns true if the callee path is a named-type default call (type in pipeline_names) or a trait Default::default() call; anything else (non-call expr) is false.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `expr_path_is_trait_default_call_expr`
- spec 3 · read at `9a6a56381894` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:53:41Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the given syn::Expr is a call expression (Expr::Call) whose callee is a path matching `Default::default`, by extracting the call's func expression and delegating the path check to expr_path_is_trait_default_call; returns false for any other expression shape.
- found: Matches Expr::Call, delegating the call's func to expr_path_is_trait_default_call; any other expression variant returns false.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `expr_path_is_named_default_call`
- spec 3 · read at `7dda0db0cf22` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:11:50Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Checks whether expr is an Expr::Call whose callee is an Expr::Path like `SomeType::default()`, where the type segment's name is a member of type_names, and the last path segment is literally "default". Returns true only in that case, false otherwise (including for non-call exprs).
- found: Matches on Expr::Path directly (not a Call), collects segment idents, checks the last segment is literally "default" and the second-to-last is in type_names — i.e. checks the path expr `Type::default` itself rather than a call expression.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `expr_path_is_trait_default_call` — QUIRKY
- spec 3 · read at `040054beb574` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:06:48Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the given expression is a call to a Default-trait style constructor via a path, e.g. `Default::default()` or `<Type as Default>::default()`. Likely inspects the expr as an Expr::Call, extracts the path segments of the function being called, and checks that the last segment is named "default" and an earlier segment/qualifier references "Default". Complements expr_path_is_named_default_call which probably checks for a plain named type's ::default() call.
- found: Checks if the expr is an Expr::Path (not a Call) matching either the 2-segment `Default::default` path or the 4-segment `std::default::Default::default` fully-qualified path — used presumably by a caller that wraps this in a Call check.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `type_contains_named`
- spec 3 · read at `efff96277498` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:44:04Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Takes a syn::Type and a set of names, and returns true if the type matches one of those names anywhere in its structure. Likely matches on Type::Path (delegating to type_path_contains_named) and Type::Reference (recursing into the referent), returning false for other variants.
- found: Recursively checks a syn::Type for a matching name: delegates Type::Path to type_path_contains_named, recurses through Type::Reference and each element of Type::Tuple, and returns false for everything else.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `type_path_contains_named`
- spec 3 · read at `5defd66467ef` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:45:18Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Iterates the path segments of `type_path` and checks if any segment's identifier (as a string) is contained in `names`, returning true on the first match — used to detect whether a type reference mentions one of a set of target type names anywhere along its path, including inside generic arguments if it recurses.
- found: Checks if the type path's last segment matches a name in `names` via path_last_segment_is_named, then recursively checks generic arguments (angle-bracketed) and parenthesized Fn-trait inputs/output for matches via type_contains_named.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `path_last_segment_is_named`
- spec 3 · read at `a24c6d68000d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:55:48Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: Gets the last segment of the syn::Path, converts its ident to a string, and returns whether that string is present in the `names` HashSet — matching a path by its final identifier regardless of full qualification/prefix.
- found: Exactly as predicted: last segment's ident stringified and checked against the names set, false if path is empty.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `has_allowance_near_line`
- spec 3 · read at `73ad6632f51e` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:51:37Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Checks whether there's an explicit "allow" marker comment (e.g. a special audit-suppression comment like "// audit-allow" or similar) within a few lines above/around the given line number in the source text, to let specific violations of this static audit be manually whitelisted. Splits source into lines, looks at a small window around `line`, and returns true if the marker substring is found there.
- found: Looks at a window of up to 4 lines ending at `line` and returns true if any of those lines contain the literal marker "settings-sentinel-allow", used to manually whitelist audit violations near a given line.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `has_cfg_test`
- spec 3 · read at `c953c678162e` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:56:37Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Iterates the given attrs looking for one whose path is "cfg", then checks whether its meta/tokens contain the identifier "test" (e.g. #[cfg(test)]), returning true if such an attribute is found so the audit visitor can skip test-only code.
- found: Checks attrs for one whose path is "cfg", then for Meta::List checks the stringified tokens contain "test", for Meta::Path checks it's exactly "test", NameValue never matches.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

## tests/subprocess_stdin_convention.rs

### the file itself
- spec 3 · read at `eddf40e99b38` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:09:03Z · by ross@rossturk.com · cold reading · reading 7 of its run · priming: CLAUDE.md excluded
- expected: A repo-convention lint test: it implements a lightweight source-scanning detector (scan_file walks a file's lines, scan_dir walks the src tree, violating_lines finds the actual offending lines) that looks for `.spawn()`/`.status()` calls not preceded/paired with an explicit `Stdio::null()` (or similar) stdin configuration, respecting an inline exemption-comment marker to skip deliberate inherits like $EDITOR. The main test (spawn_and_status_subprocesses_configure_stdin) runs this detector over the actual codebase and fails if any subprocess launch violates the convention; the rest of the peers are unit tests exercising the detector's parsing logic on synthetic snippets (single-statement chains, builder patterns, exemption markers, multiple violations, unrelated code in between).
- found: Text-window based detector scanning source for .spawn()/.status() calls not preceded by .stdin( within a capped window back to Command::new(, honoring a DELIBERATE-marker exemption and skipping Commands consumed by .output(); the integration test scans the whole workspace (derived from Cargo.toml members) and fails on any violation, backed by unit tests pinning the detector's own logic.
- predicted: full · documented: full · derivable: no · legible: not judged · trap: no
- note: The header's explanation of *why* (past hangs) and the window-cap blind spot with the repackage helper are not derivable from code alone and are valuable context.

### `scan_file`
- spec 3 · read at `31f4972cde60` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:08:39Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: This reads the file at path, calls violating_lines (or similar) to detect .spawn()/.status() calls lacking explicit stdin configuration, and pushes formatted violation strings (with file/line info) into the violations vector — a source-scanning check used by a sentinel test rather than actual runtime behavior.
- found: Reads the file's source text, runs violating_lines over it to find line numbers of spawn/status calls without explicit stdin configuration, and appends "path:line" strings to the violations vector.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `violating_lines`
- spec 3 · read at `ba26ba785db2` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:53:54Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Heuristic text scanner over Rust source: finds `.spawn()`/`.status()` call sites, walks backward/forward within the same statement (handling multi-line builder chains) to check whether `.stdin(...)` was configured, and skips call sites with an exemption comment marker above them. Returns line numbers of calls that implicitly inherit stdin. Likely uses line/string-based heuristics rather than full parsing, to handle chained builder patterns and multiple commands in a range.
- found: For each `.spawn()`/`.status()` occurrence, finds the nearest preceding `Command::new(` within a bounded window (skipping if none, since it's not a real Command), skips if `.stdin(` appears in that window or an exemption marker comment appears just above the Command, skips if `.output()` appears in the window (meaning the Command was already consumed by a different call), and otherwise records the 1-indexed line number of the launch.
- predicted: most · documented: most · derivable: no · legible: most · trap: no

### `scan_dir`
- spec 3 · read at `67c9fd8773a3` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:58:53Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Recursively walks the given directory, reading its entries; for each subdirectory it recurses into scan_dir, and for each Rust source file it calls scan_file to detect stdin-convention violations, appending any found violation messages to the shared violations vector.
- found: Recursively reads directory entries, recursing into subdirectories and calling scan_file on any .rs file, accumulating violations into the shared vector.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `spawn_and_status_subprocesses_configure_stdin`
- spec 3 · read at `2ae3301863f0` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:20:31Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This is the real enforcement test (as opposed to the detector unit tests among its peers): it calls scan_dir over the actual project source tree (likely the crate root or src/) and asserts that violating_lines comes back empty, so CI fails if someone adds a new .spawn()/.status() call that doesn't configure stdin.
- found: Scans src/ plus every workspace member's src/ (parsed from Cargo.toml's members line) for stdin-inheriting subprocess launches, asserts at least 8 members were scanned (guards against a broken parse silently scanning nothing), and asserts no violations were found.
- predicted: most · documented: most · derivable: no · legible: full · trap: no

### `detector_flags_single_statement_chain`
- spec 3 · read at `b64753792d21` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:51:47Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Feeds a small snippet of source containing a single-statement chained call like Command::new(\"foo\").spawn()?; (no .stdin(...) in the chain) to violating_lines (or scan_file), and asserts the detector flags exactly one violation at that line.
- found: Builds a two-line source snippet with a single-statement Command::new(\"x\").arg(\"y\").status() chain lacking stdin configuration, and asserts violating_lines flags line 2.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `detector_flags_builder_pattern_spawn`
- spec 3 · read at `4a952e945689` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:22:31Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Feeds source text where a Command is built via chained builder calls spanning multiple lines (e.g. Command::new(...) on one line, .arg(...) on another, .spawn() on a third) with no stdin configuration, into the detector (scan_file/violating_lines), and asserts it still flags the .spawn() call as a violation despite the multi-line chain.
- found: Builds a source snippet where a Command is assigned to a variable (`let mut cmd = Command::new(...)`), then `.arg()` and `.spawn()` are called on it as separate statements with no stdin configuration, and asserts violating_lines returns exactly [4], the line containing .spawn().
- predicted: most · documented: most · derivable: no · legible: full · trap: no
- note: I predicted a chained builder call across lines; actual case is a variable-bound Command with separate statements per method.

### `detector_passes_stdin_configured_launches`
- spec 3 · read at `c2681e2d1e18` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T21:13:46Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A #[test] that feeds a small Rust source snippet with .spawn()/.status() calls that properly configure .stdin(Stdio::null()) into the detector (via scan_file/violating_lines), asserting it reports zero violations — the clean-case counterpart to the detector_flags_* tests.
- found: Tests two clean cases (single-statement chain, and builder-pattern with separate .stdin() call) both asserting violating_lines is empty, exactly as predicted.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `detector_honors_exemption_marker_above_the_launch`
- spec 3 · read at `c7596bbdd0ea` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:27:35Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A test with an inline source snippet containing a .spawn() or .status() call preceded by a special exemption comment marker, asserting that violating_lines/scan_file returns no violations for that snippet because the marker suppresses the detection.
- found: Inline source snippet with a "DELIBERATE stdin inheritance" comment directly above a .status() call, asserting violating_lines returns empty because the marker exempts it.
- predicted: full · documented: most · derivable: no · legible: full · trap: no

### `detector_skips_output_consumed_command_before_unrelated_status` — OBSCURE
- spec 3 · read at `0dbe57eb77c6` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:06:35Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A test with a source snippet containing a Command::new(...).output() call (exempt) followed by a separate, unrelated Command::new(...).status() call lacking stdin configuration. It asserts the detector's violating_lines correctly flags only the status() line as a violation and does not mistakenly associate/skip it because of the preceding output() call.
- found: Asserts the detector does not flag `resp.status()` as an unconfigured-stdin Command violation just because an unrelated Command::output() call appears on the preceding line — violating_lines must be empty.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no

### `detector_ignores_status_with_no_command_in_range` — QUIRKY
- spec 3 · read at `4969a9ed5f9c` · commit `1681528` · read by claude-sonnet-5 · when 2026-08-19T08:33:08Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: This test feeds the detector a source snippet where `.status()` is called but no `Command::new(...)` construction appears within the lookback window/range the detector scans, so the detector should NOT flag it as a violation (no false positive when the Command being invoked isn't visible nearby). It asserts the resulting violations list is empty.
- found: Asserts the detector produces zero violations for a snippet with `resp.status()` (an HTTP response, not a subprocess) and `tokio::spawn(async {})` (an async task, not process::Command::spawn) — the detector must not be fooled by method-name collisions with unrelated types.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `detector_flags_each_violation_with_its_own_line`
- spec 3 · read at `f7fd049d646d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T17:55:24Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: This test builds a small snippet of Rust source containing two separate .spawn()/.status() calls without stdin configured, runs the detector (likely violating_lines or scan_file) on it, and asserts that it returns two distinct line numbers—one per violation—rather than merging them into a single report.
- found: Builds a source snippet with a direct .status() call missing stdin config (line 1), a compliant .status() call with stdin(Stdio::null()) (line 2), and a builder-then-.spawn() call missing stdin config split across two statements (lines 3-4). Asserts violating_lines returns [1, 4] — the line of each violating call, confirming the compliant one is excluded and each violation is reported at its own line even when the builder pattern spans a separate statement.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

## tests/tui_format_pipeline_settings.rs

### the file itself
- spec 3 · read at `abe5866372d1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:15:36Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Regression-test suite for the TUI's dynamic format-selection state (PCM/DSD/DSF/DFF, presets, dither), verifying that the in-memory format state maps correctly and losslessly to PipelineSettings, that hidden/suppressed fields (e.g. PCM-only fields when DSD is selected) don't leak into pipeline settings, that UI navigation skips hidden rows, that presets (including legacy v2->v3 migration) round-trip stable format keys rather than display labels, and that auto-dither logic picks sensible defaults without guessing on unknown source depths. The doc note suggests it's an external test file because TUI modules are currently private, with a migration note for later.
- found: Confirms: tests that FormatState (PCM/DSD format selection in the TUI) maps losslessly to PipelineSettings, that DSD selection suppresses PCM/replaygain/dither fields, that pills-to-options keeps legacy fields consistent, auto-dither default selection based on source bit depth, hidden-row navigation per format family, stable format keys (vs display labels) in presets, v3 preset round-tripping of new DSD fields, v2 preset loading with v3 defaults, and a comprehensive sweep over all format families for visible rows and valid pipeline mapping.
- predicted: full · documented: some · derivable: no · legible: not judged · trap: no

### `config`
- spec 3 · read at `f3d93580a643` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:51:55Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: A shared test fixture helper that returns a default/minimal TonepoetConfig instance (likely via TonepoetConfig::default() or similar) for use across these TUI format pipeline settings tests.
- found: Returns TonepoetConfig::default(), a one-line fixture helper.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `pcm_format_state_maps_to_pipeline_settings_without_loss`
- spec 3 · read at `64d644bd5bd4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:08:14Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Constructs a TUI format state configured for a PCM output (e.g. specific bit depth, sample rate, dither settings), runs it through the conversion function that produces PipelineSettings, and asserts that each configured field (bit depth, sample rate, dither type, etc.) round-trips into the resulting PipelineSettings exactly, with nothing silently dropped or defaulted.
- found: Sets FLAC format, 96kHz sample rate, 24-bit depth, SSRC resampler, TPDF dither, and Both replaygain on a FormatState, calls apply_format_constraints, converts via format_state_to_pipeline_settings, and asserts each field maps to its PipelineSettings equivalent (including the nyquist_transition BrickWall which is a derived/implicit field, not one directly set), then validates the result.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The nyquist_transition BrickWall assertion is a derived value from the dither/resampler choice, not a direct 1:1 field mapping I predicted.

### `dsd_format_state_suppresses_hidden_pcm_and_replaygain_state`
- spec 3 · read at `77056eb07c00` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:25:05Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Constructs a format state with a DSD target (e.g. DSF/DFF) and asserts that when mapped to pipeline settings, PCM-only fields (bit depth, dither, resampler) and ReplayGain fields are omitted/None/disabled since those controls are hidden in the DSD UI, rather than carrying over stale PCM values.
- found: Sets replaygain and dither selections plus DSD format/rate/noise-shaper/modulator/filter, maps to pipeline settings, and asserts dither is forced to None, replay_gain.mode is None, bit depth is Source, while the DSD-specific pcm_to_dsd fields (noise shaper, modulator order, filter) pass through unchanged; also asserts the resulting settings validate.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `pills_to_options_keeps_legacy_fields_consistent_for_dsd`
- spec 3 · read at `aab97939d036` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:29:39Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Test that configures TUI format state to select a DSD output format via the pill selectors, converts it to pipeline options, and asserts that legacy/deprecated option fields (kept around for backward compatibility, e.g. old bit-depth or format-string fields) remain consistent with the new DSD-specific fields rather than being left stale or contradictory — e.g. legacy PCM-oriented fields are cleared or mirror the DSD selection instead of holding leftover PCM values.
- found: Sets up FormatState for a DSD (Dff) selection with a DSD sample rate, applies format constraints, converts to options via try_pills_to_options, and asserts PCM-only legacy fields (target_bit_depth, dither_type, calculate_replaygain/replaygain_mode) are cleared to None/false while output_format is Dff and pipeline_settings is populated.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The test also explicitly sets dither and replaygain pill values before conversion, which the docs give no hint of — worth knowing that DSD selection is expected to override/ignore those inputs rather than reject them upfront.

### `auto_dither_selects_defaults_and_preserves_manual_choice`
- spec 3 · read at `c35349803825` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:11:08Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: Builds a format/pipeline state with dither set to "Auto" and asserts the pipeline settings resolve to a sensible default (e.g., dither enabled when reducing bit depth). Then sets dither to an explicit manual value and asserts that value is preserved unchanged rather than being overridden by the auto-selection logic.
- found: Tests that auto-dither selects different default dither types depending on the bit-depth reduction (Shibata for 24->16, TPDF for 32->24, None when not reducing e.g. 24->32), and that after explicitly selecting a dither type and calling mark_dither_overridden(), further bit-depth changes no longer override the manual choice.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `format_navigation_skips_hidden_rows` — OBSCURE
- spec 3 · read at `1b0798ebeb45` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T01:16:09Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Sets up a FormatState (or similar pill/list) where certain rows are hidden depending on current format family selection (e.g. PCM-only rows hidden while DSD selected, or vice versa). Then calls a navigation method (next/move_down or similar) repeatedly and asserts the selected/focused row index lands only on visible rows, never stopping on a hidden one.
- found: No cursor/navigation simulation at all — it just calls FormatField::visible_rows(dsd, ..., ...) for PCM and DSD configurations and asserts which fields (Resampler, Dither, NoiseShaper, DsdRate) are present/absent in each set, confirming the visibility predicate itself, not any navigation-skipping behavior.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: Test name implies navigation/cursor behavior ("skips hidden rows") but the test only checks the static visible_rows() membership set — the actual skip-during-navigation logic (if it exists) is untested here.

### `format_pill_contains_distinct_dsf_and_dff_targets` — QUIRKY
- spec 3 · read at `0ebef0c42de2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:34:52Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: Builds format state/config and asserts that the rendered format-pill options/labels include both DSF and DFF as separate, distinct entries (not merged into a single DSD choice), verifying the UI exposes both container formats independently.
- found: Actually selects Dsf then Dff on a FormatState's format field one at a time, asserting selected_value() and is_dsd_selected() each time — not a "list contains both as options" check as I predicted, but a sequential select-and-verify-identity test for each of the two distinct values.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `preset_uses_stable_dsf_and_dff_format_keys_not_display_labels`
- spec 3 · read at `cf0f624ab0a4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:31:25Z · by ross@rossturk.com · cold reading · reading 2 of its run · priming: CLAUDE.md excluded
- expected: This test creates a format pill/state for DSF and DFF targets, serializes it into a preset, and asserts the stored format key is a stable identifier (e.g. "dsf"/"dff") rather than a human-readable display label (e.g. "DSF (DSD)"), so that changing UI labels doesn't break saved presets.
- found: For DSF and DFF, builds a FormatState, creates a TuiPreset via from_pill_state, asserts preset.format equals the format's extension (stable key), then round-trips through TOML encode/decode and re-applies to fresh pill state, asserting the restored format and DSD-selected flag match the original.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `preset_v3_round_trips_new_format_fields`
- spec 3 · read at `5aa390cb4a70` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:05:29Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Builds a TUI format/pipeline state with the new v3-only fields populated, serializes it into a preset, then deserializes that preset back into state and asserts each new field matches the original values exactly, confirming no loss when round-tripping through the v3 preset format.
- found: Sets DSF format with DSD-specific fields (sample rate, noise shaper, modulator order, conversion preset, resampler) plus a merge mode output option, builds a TuiPreset, round-trips it through TOML serialize/deserialize, applies the decoded preset to fresh state objects, and asserts each new field matches the original.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `preset_v2_loads_with_v3_defaults`
- spec 3 · read at `d1d33951d39d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:00:39Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Constructs/loads a serialized "preset v2" (older format lacking newer v3 fields), deserializes it into the current settings struct, and asserts that the missing v3-only fields get their expected default values while the v2 fields are preserved correctly — verifying backward-compatible preset loading doesn't panic or silently corrupt state.
- found: Parses a hardcoded TOML string representing a v2-era TuiPreset (no resampler/noise_shaper/modulator_order/dsd_filter_preset fields) via toml::from_str, then asserts the deserialized struct fills resampler with default \"sox\" and leaves the other three new fields as None.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `auto_dither_unknown_source_depth_does_not_guess_reduction`
- spec 3 · read at `e89acf6609ef` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:42:03Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Sets up a format/pipeline state with auto-dither enabled but source bit depth unknown/unset, maps it to pipeline settings, and asserts no dither reduction is guessed — dithering stays off/None rather than assuming a default source depth.
- found: Calls state.select_bit_depth with an unknown source depth (None) for both Int24 and Int16 targets, asserting dither.selected_value() stays DitherType::None in each case, confirming the auto-dither logic doesn't guess a reduction amount when it doesn't know the source depth.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `all_format_families_have_expected_visible_rows_and_valid_pipeline_mapping`
- spec 3 · read at `4b1334368765` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:44:57Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Iterates over every format family the TUI supports (e.g. FLAC, WAV/PCM, DSF, DFF, etc.), constructs the format state for each, asserts the visible row count matches an expected value per family (since some rows are hidden depending on format, echoing the sibling tests about hidden rows), and asserts the state converts into pipeline settings without error/loss for every family — a table-driven sweep rather than one specific case.
- found: Table-driven sweep over 10 formats with an is_dsd flag; for each it checks specific field visibility (Resampler/Dither/ReplayGain for PCM vs DsdRate/NoiseShaper/ModulatorOrder/ConversionPreset for DSD), checks that PCM vs DSD sample-rate options are enabled appropriately (with a source-rate sentinel excluded), and validates the pipeline settings conversion succeeds.
- predicted: most · documented: none · derivable: no · legible: most · trap: no

## tests/unified_synthetic_cue_output_boundary.rs

### the file itself
- spec 3 · read at `18d4d4fc5b5c` · commit `1681528` · read by claude-sonnet-4.5 · asked for claude-sonnet-5 · via claude · when 2026-08-20T07:15:11Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Integration-test file exercising the "synthetic CUE" merge pipeline end-to-end through the real public pipeline entrypoint, using real ffmpeg/sox/metaflac binaries (skipping tests if unavailable) rather than mocking. Contains helper functions for: unique temp dirs, checking/skipping on missing boundary tools, setting/reading FLAC tags and cuesheets, synthesizing sine-wave FLAC audio fixtures, building a base pipeline request, and asserting published directory contents. Followed by three #[test] functions: (1) a folder of multiple audio files that generates a synthetic multi-FILE CUE publishes as one unified album directory, (2) an explicit single-side CUE stays a bypass and keeps its own album identity rather than merging, (3) embedded metadata on the synthetic album drives the folder naming template and is correctly written into real output file tags.
- found: A module-doc-only test file with helper functions (unique temp roots, PATH tool checks, FLAC tag/cuesheet read-write via lofty, sine-wave FLAC fixture synthesis via ffmpeg, a full PipelineRequest builder, and directory/publish assertions) followed by three #[tokio::test] functions: folder_expansion_generated_synthetic_cue_publishes_one_album_boundary (multi-file folder expansion generates one synthetic CUE that publishes as a single merged album dir with one conversion.log/durable JSON log), explicit_single_cue_bypass_keeps_side_album_identity (an explicit single-side CUE publishes under its own side-specific album identity, not merged), and embedded_unified_album_metadata_drives_folder_template_and_real_output_tags (embedded per-file metadata is propagated into the synthetic CUE, drives a custom folder_template split into base album vs title-extra, and is verified in real written FLAC tags including MusicBrainz IDs).
- predicted: most · documented: most · derivable: no · legible: most · trap: no
- note: The module doc explains the *why* (public-entrypoint testing, real binaries over ToolRunner mocking) accurately but says nothing about the folder-template title-extra splitting behavior tested in the third test, which is a fairly novel/specific piece of pipeline logic.

### `unique_root`
- spec 3 · read at `9331c7d64827` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:27:14Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Builds a unique root PathBuf under std::env::temp_dir(), combining the label with a nanosecond timestamp and process id to avoid collisions across parallel test runs, similar to the unique_dir helper seen elsewhere.
- found: Builds a PathBuf under std::env::temp_dir() named "tonepoet-{label}-{nanos}" using only a nanosecond timestamp (unwrap_or_default, not process id) as the uniquifier.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `executable_on_path`
- spec 3 · read at `5cb304694145` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:18:16Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Checks whether an executable named `name` (e.g. ffmpeg, sox, flac) is available on the system PATH, likely by iterating PATH directories and checking for a file of that name (or by invoking `which`), returning true/false for use by boundary_tools_available gating helpers that decide whether to skip tests requiring real binaries.
- found: Reads the PATH env var, splits it into directories, and checks whether any directory contains a file named `name`, returning false if PATH is unset.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `boundary_tools_available`
- spec 3 · read at `ddf89b4e85fc` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:32:37Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Checks whether the external binaries this test needs (likely ffmpeg and sox, given the file_doc mentions real streaming binaries) are present on PATH, probably by calling executable_on_path for each and ANDing the results together, returning true only if all are available.
- found: Checks ffmpeg, ffprobe, and sox are all on PATH via executable_on_path, ANDed together.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `metadata_boundary_tools_available` — OBSCURE
- spec 3 · read at `2f59b272694c` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:51:57Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Combines boundary_tools_available() (ffmpeg/sox on PATH) with an additional executable_on_path check for a metadata tool such as metaflac needed for set_flac_tags/read_flac_tags, returning true only if both the encoding tools and the tag tool are present.
- found: Simply returns boundary_tools_available() unchanged — a thin same-named wrapper with no added metadata-tool check.
- predicted: none · documented: none · derivable: yes · legible: full · trap: no
- note: The name implies an additional metadata-tool (e.g. metaflac) check that the body doesn't actually perform — it's just an alias for boundary_tools_available.

### `require_or_skip_boundary_tools`
- spec 3 · read at `180cef07a13c` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:57:05Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: Calls boundary_tools_available() to check whether the required real binaries (ffmpeg/sox) are on PATH; if not, prints/eprintln a skip notice mentioning test_name and returns false so the caller can early-return, otherwise returns true.
- found: Returns true if boundary_tools_available(); otherwise checks TONEPOET_REQUIRE_TOOLS env var — if set/truthy, panics demanding the tools, else eprintln's a skip notice and returns false.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `require_or_skip_metadata_boundary_tools`
- spec 3 · read at `067b1d962c94` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T07:09:12Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Calls metadata_boundary_tools_available() to check whether the needed external binaries (e.g. ffmpeg/sox/metaflac) are on PATH; if not available, prints/logs a skip message including test_name and returns false so the caller can early-return (skip the test), otherwise returns true so the test proceeds.
- found: Returns true if metadata_boundary_tools_available(); otherwise checks TONEPOET_REQUIRE_TOOLS env var — if set/truthy, panics demanding ffmpeg/ffprobe/sox; otherwise eprintln's a skip message and returns false.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `set_flac_tags`
- spec 3 · read at `5e31125c43c1` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:16:25Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Test helper that opens the FLAC file at `path` (via lofty), gets or creates its primary tag, and for each (key, value) pair in `tags` sets a Vorbis comment item with that name/value, then saves the file back to disk. Likely panics/unwraps on failure since this is test setup.
- found: Reads FLAC via lofty, creates a primary tag if absent, then for each key/value removes any existing ItemKey::Unknown(key) and inserts it fresh with the text value, and saves back to path, panicking on any failure.
- predicted: most · documented: none · derivable: no · legible: full · trap: no

### `set_flac_cuesheet` — QUIRKY
- spec 3 · read at `65744372736f` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:57:18Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Shells out to the metaflac binary via std::process::Command to set/import a CUESHEET tag or block on the FLAC file at `path`, passing cue_text as the cuesheet content, and panics/asserts on failure since this is test scaffolding.
- found: Delegates to set_flac_tags, setting CUESHEET as a plain Vorbis comment tag rather than shelling out separately or using a cuesheet-import mechanism.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `item_key_matches_vorbis_name`
- spec 3 · read at `4e0ab1c03237` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:23:19Z · by ross@rossturk.com · cold reading · reading 10 of its run · priming: CLAUDE.md excluded
- expected: A small helper used when reading back FLAC tags for assertions: maps the lofty ItemKey to its Vorbis Comments field name (via key.map_key(TagType::VorbisComments, ...) or similar) and compares it case-insensitively to `name`, returning true on match so tests can find/assert a specific tag field like "ARTIST" or "TITLE".
- found: Checks first if the key is ItemKey::Unknown with a raw string matching `name` case-insensitively (short-circuit true), otherwise maps the key to its Vorbis Comments name via map_key and compares case-insensitively.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `read_flac_tags` — QUIRKY
- spec 3 · read at `43dfc9611c0e` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:01:07Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Shells out to a metadata tool (likely `metaflac --list` or `--export-tags-to`) on the given FLAC file, parses its output, and builds a HashMap of the requested tag keys (case-insensitive Vorbis comment names) to their string values. Only keys present in `keys` are included; missing tags are simply absent from the map.
- found: Uses the lofty crate to read the file's primary (or first) tag in-process, then for each requested key finds a matching item via item_key_matches_vorbis_name and extracts its text/locator value into a HashMap, skipping binary values and missing keys.
- predicted: some · documented: none · derivable: yes · legible: full · trap: no

### `create_sine_flac`
- spec 3 · read at `d30908cb4c6d` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T18:03:21Z · by ross@rossturk.com · cold reading · reading 6 of its run · priming: CLAUDE.md excluded
- expected: Shells out to a real external tool (likely `sox`, given sox is used elsewhere as the encoder) with a "synth" sine-wave generator argument and duration_secs, encoding directly to a FLAC file at the given path; it probably asserts/expects the command succeeds (panicking otherwise) since this is test fixture setup that must produce a real playable FLAC for downstream ffmpeg/sox pipeline stages to operate on.
- found: Shells out to ffmpeg with a lavfi sine generator (1kHz, 44.1kHz) piped through the flac encoder to produce a real FLAC file at path, panicking/asserting on failure with stderr included.
- predicted: most · documented: full · derivable: no · legible: full · trap: no

### `base_request`
- spec 3 · read at `a93114c4c530` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:03:47Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: A test helper that constructs a fully-populated PipelineRequest with sensible default settings (source container path, output root, log root, plus default encode/format/downmix settings) so individual tests only need to override the fields relevant to what they're checking.
- found: Builds a full PipelineRequest literal with explicit defaults for every field: source options (no dvda/bluray selections, sidecar-only CUE policy, all tracks), default pipeline settings, naming template, fail-fast publish/naming/failure policies, JSON+conversion logging enabled, metadata/replaygain stages disabled but features enabled, and a companion-copy policy that copies .log files alongside output.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no
- note: The specific stage/failure-policy defaults (metadata+replaygain disabled, FailAlbumOnAnyTrackFailure, companion .log copying) are load-bearing for what this boundary test is actually checking and aren't guessable from the name alone.

### `visible_dirs`
- spec 3 · read at `6d44842c2f73` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:52:00Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Reads the entries of root, filters to directories only, excludes hidden ones (names starting with '.'), and returns their names (not full paths) as a Vec<String>, likely sorted for deterministic test assertions.
- found: Reads root's directory entries, filters to non-hidden directories, collects their names into a Vec<String>, and sorts it.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `assert_album_dir_contains_exact_audio_files_and_no_subdirs`
- spec 3 · read at `dbda078da133` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T08:11:04Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Lists the entries in album_dir, counts audio files (likely by extension) and checks there are exactly `expected` of them, and separately asserts no subdirectories exist within album_dir — failing the test with a descriptive message if either check fails.
- found: Reads album_dir entries, asserts no subdirectories exist, filters files by a fixed list of audio extensions (case-insensitive), and asserts the count equals `expected`, with descriptive panic messages on failure.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `count_files_matching`
- spec 3 · read at `d97801322126` · commit `1681528` · read by claude-sonnet-5 · asked for sonnet · via claude · when 2026-08-19T06:08:52Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Recursively walks the directory tree rooted at `root` (via fs::read_dir, manually recursing into subdirectories), and counts how many regular files satisfy the given `predicate` closure on their path, returning the total count as usize — a generic test helper for asserting output directory contents.
- found: Iterative (stack-based) recursive walk of the directory tree from root, pushing subdirectories back onto the stack and counting files where predicate(path) is true; silently skips unreadable directories.
- predicted: full · documented: none · derivable: no · legible: full · trap: no

### `count_files_named`
- spec 3 · read at `9c332ff697f4` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:23:23Z · by ross@rossturk.com · cold reading · reading 1 of its run · priming: CLAUDE.md excluded
- expected: Recursively walks the directory tree rooted at `root` and counts how many entries have a filename exactly equal to `name` (likely via walkdir or manual recursion), returning the count as usize.
- found: Delegates recursion to count_files_matching with a predicate comparing the file's base name (via file_name/to_str) to the given name string.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `count_json_files`
- spec 3 · read at `4f3d596f18e2` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T07:30:11Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: Recursively walks the directory tree at root and returns the count of files with a .json extension, used by the tests to assert exactly one album-level log/companion JSON file was published rather than one per track.
- found: Delegates to count_files_matching with a predicate checking for .json extension, rather than implementing the tree walk itself.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `published_entries_named`
- spec 3 · read at `cd7008ad442a` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T01:14:07Z · by ross@rossturk.com · cold reading · reading 9 of its run · priming: CLAUDE.md excluded
- expected: A small test helper that counts how many entries within a PublishedAlbum result (files or tracks produced by the pipeline) have a given name, used to assert e.g. that exactly one album-level log/companion file with a specific name was published, supporting the module's assertions about merged vs bypass CUE publishing.
- found: Counts entries in published.entries whose final_path's file name matches the given name string exactly.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `published_audio_entries`
- spec 3 · read at `e3d61d68a194` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T02:36:41Z · by ross@rossturk.com · cold reading · reading 8 of its run · priming: CLAUDE.md excluded
- expected: A small test-helper that filters PublishedAlbum's entries down to just the audio files (excluding logs, companion JSON, cuesheets, etc.), likely by checking file extension or an entry-kind flag, and returns them as a Vec of references.
- found: Filters published.entries to those whose role is PublishRole::Audio, returning references as a Vec.
- predicted: most · documented: none · derivable: yes · legible: full · trap: no

### `folder_expansion_generated_synthetic_cue_publishes_one_album_boundary`
- spec 3 · read at `62e54920adc5` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-20T00:40:41Z · by ross@rossturk.com · cold reading · reading 4 of its run · priming: CLAUDE.md excluded
- expected: Creates several synthetic FLAC "side" files (via create_sine_flac) in a folder without an explicit CUE, runs them through the real production pipeline (base_request + actual ffmpeg/sox), and asserts the folder-expansion logic auto-generates a merged synthetic CUE that publishes as a single album directory — checking with assert_album_dir_contains_exact_audio_files_and_no_subdirs and counting log/companion files that there's exactly one album-level log rather than one per source file.
- found: Creates two side FLACs each with its own explicit per-side CUE sheet plus a companion rip.log, runs folder expansion which merges them into one planner-generated synthetic CUE artifact, then runs that through the real production pipeline and asserts the result publishes as a single 'The Album' directory containing all 4 tracks, one conversion.log, one rip.log, one durable JSON log, and one terminal pipeline event — i.e. no per-side directory split.
- predicted: most · documented: most · derivable: no · legible: full · trap: no
- note: I predicted no explicit CUE at all, but the source actually has two explicit per-side CUEs that folder expansion merges into a synthetic one — the 'synthetic CUE' terminology refers to the merged artifact, not the absence of source CUEs.

### `explicit_single_cue_bypass_keeps_side_album_identity`
- spec 3 · read at `dccca567571e` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T22:20:53Z · by ross@rossturk.com · cold reading · reading 3 of its run · priming: CLAUDE.md excluded
- expected: Sets up a source directory with an explicit (non-synthetic) single-side CUE sheet alongside its audio file, runs it through the real production pipeline entry point, and asserts that the output is published under its own side-specific album directory (not merged/synthesized as a multi-FILE album), verifying the explicit-CUE bypass path preserves per-side album identity rather than triggering the synthetic-CUE merge behavior.
- found: Creates a single explicit CUE ("side_a.cue") referencing one FLAC file with two tracks, runs it through the real pipeline, and asserts it publishes as its own "The Album Side A" directory (not merged into "The Album"), with 2 audio files and 1 conversion log.
- predicted: full · documented: none · derivable: yes · legible: full · trap: no

### `embedded_unified_album_metadata_drives_folder_template_and_real_output_tags`
- spec 3 · read at `67d69f85e7a1` · commit `1681528` · read by claude-sonnet-5 · via claude · when 2026-08-19T23:58:46Z · by ross@rossturk.com · cold reading · reading 5 of its run · priming: CLAUDE.md excluded
- expected: Creates a synthetic sine-wave FLAC with embedded cuesheet/tags representing unified album metadata, runs it through the real production pipeline entry point with a folder-name template referencing album metadata fields, then asserts both that the output directory name expanded correctly and that the real encoded output file's tags (via read_flac_tags) carry the same album-level metadata — an end-to-end metadata-flow check.
- found: Builds two-side FLAC sources with per-side and embedded merged cuesheets/tags (full pressing title, MusicBrainz IDs, catalog number), expands to a synthetic unified CUE, runs the real pipeline with a folder template splitting base album vs. bracketed title-extra, and asserts both the merged album directory name and the real published FLAC's propagated tags (album, catalog, country, year, MB IDs).
- predicted: most · documented: most · derivable: no · legible: most · trap: no
- note: The file_doc explains the overall intent (real pipeline, not predicted names) but not this specific test's folder-template splitting or the exact tag set asserted.
