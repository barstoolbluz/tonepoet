# Triage brief: 9 pre-existing test failures hidden by a broken test build

Date: 2026-07-07

## Background — how these went unnoticed

The test build (`cargo test`) at commit `549040b` did not compile: `title_extra_tests` in `src/convert/pipeline/stages.rs` called `template_source()` / `template_request()` from the sibling `naming_template_tests` module without visibility or an import (16 compile errors). Because the lib test binary failed to build, **no lib tests ran** in the sessions that landed the recent feature commits, and `cargo test`'s fail-fast behavior also meant integration binaries were skipped.

The visibility errors are now fixed (`pub(super)` on the two helpers plus an import in `title_extra_tests`), which surfaced 9 test failures that pre-date the udfread crash bundle. Each failure below was introduced with its feature commit and has never passed.

These are all cases where the test's expectation and the implementation disagree. You wrote both sides (in different bundles); decide which side is correct and fix that side.

## Failures

### 1. `title_extra_tests::create_disc_subfolders_option_reaches_planned_final_paths` (stages.rs)
From `92b2d4e` (disc subfolders). Expected `plan.entries[2].final_path` = `/out/Disc 01/Miles Davis/2 - Done Somebody Wrong.flac`; actual `1 - Done Somebody Wrong.flac`. The `%TRACK%` token in the planned final path renders `1` where the test expects `2` — track numbering within inferred disc groups appears to renumber (or not renumber) differently than the test assumes. The fixture is a 4-track, 2-disc source with explicit per-track disc numbers.

### 2. `title_extra_tests::album_total_discs_without_track_disc_numbers_does_not_infer_disc_folders` (stages.rs)
From `ac59b26`. Fixture: `total_discs = Some(2)` but every track has `disc_number = None` and sequential track numbers 1–3. Expected `%NN%` to render the track's own number (`02 - Middle`); actual `01 - Middle`. Same renumbering divergence as #1: the renderer appears to assign per-disc ordinals even when no disc inference should occur.

### 3. `title_extra_tests::duplicate_track_numbers_alone_do_not_infer_disc_folders` (stages.rs)
From `ac59b26`. Same symptom as #2 (`01 - In Memory of Elizabeth Reed` vs expected `02 - ...`) for a fixture with duplicate track numbers but no disc metadata.

### 4. `title_extra_tests::conditional_template_preserves_literal_braces_when_block_resolves` (stages.rs)
From `92b2d4e` (conditional template braces). Template: `%ARTIST% - %ALBUM% (%YEAR%) [%FORMAT%] {%TITLE_EXTRA%}` with the shared `template_source()` fixture. Expected `Miles Davis - At Fillmore East () [FLAC] {MFSL}`; actual `... (1971) ...`. Note the shared fixture *does* carry year 1971, so the expected string with an empty `()` looks like a fixture oversight in the test rather than an implementation bug — but confirm against the conditional-block semantics you intended (should a `(%YEAR%)` group with a resolving variable render the year, and should the empty-block-dropping rule have removed `()` if year were empty?).

### 5. `naming_template_tests::render_track_template_expands_new_builtins_and_custom_extras` (stages.rs)
From `243d508` (naming template expansion). `%NONEXISTENT%` at end of template expands to empty; expected output preserves the resulting trailing space (`"... - CAT 999 - "`), actual output is trimmed (`"... - CAT 999 -"`). Decide whether trailing-whitespace trimming of rendered path components is intended (it likely is, for filesystem hygiene) and fix the test, or fix the renderer.

### 6. `title_extra_tests::preserves_country_only_parenthetical` (stages.rs)
From `f0d8c7e` (%TITLE_EXTRA%). `extract_title_extra("Aftermath (US)")` should return `None` (country-only parentheticals must stay in the album title); it returns `Some(...)`. Note the sibling test `strips_last_parenthetical_only` passes `"Aftermath (US) (ABKCO Hybrid SACD ISO)"` and expects `(US)` preserved — so the country allowlist works in the two-parenthetical case but the single `(US)` parenthetical is still being extracted.

### 7. `chunk_2_1_3_postprocessing_gate_and_phase_tests::failed_publish_preserves_staging_parent_for_diagnostics` (stages.rs:26494)
Assertion: "failed publish keeps staging root for diagnostics/retry". After a forced publish failure the staging parent directory no longer exists (or is cleaned) where the test expects it preserved. Interacts with the scratch/tmpfs staging cleanup work.

### 8. `chunk_2_1_3_postprocessing_gate_and_phase_tests::real_plan_output_failure_publishes_fragment_and_completes_batch` (stages.rs:25930)
The test expects a path-escaping naming template (one that would escape the destination root) to fail during output planning; `plan_outputs` now succeeds and returns a sanitized in-root path (`.../Test Album/escaped-plan-output.flac`). Either the planner's escape handling changed from fail to sanitize (then the test should assert sanitization), or the sanitization silently swallows what should be a planning error.

### 9. `output_options_companion_projection_tests::output_options_field_cycle_includes_companion_fields_when_maximized` (src/tui/app.rs:3828)
From `e606eda` (companion copying), broken by later feature work: `MAXIMIZED_FIELDS` now inserts `ExcludeFiles`, `ForceEncode`, and `DiscSubfolders` between `CompanionFolders` and `WriteLog`; the test predates all three. Almost certainly a stale test: update the expected cycle to walk `CompanionFolders → ExcludeFiles → ForceEncode → DiscSubfolders → WriteLog` (also check the `prev_for` assertions' symmetry). Note the src/tui/app.rs line references drift as fields are added — locate `MAXIMIZED_FIELDS` by name.

## Real-world failure: folder album batch loses disc 1's subfolder

Empirical repro (2026-07-07, user's library). Source folder:

```
The Allman Brothers Band - At Fillmore East (1971) [FLAC] {MFSL}/
├── disc 1/01 - Statesboro Blues.flac … 04 - You Don’t Love Me.flac
└── disc 2/01 - Hot ’Lanta.flac … 03 - Whipping Post.flac
```

Converted with `create_disc_subfolders` (filename template `%DISC_FOLDER%/%TRACKNN% - %TITLE%.%EXT%`). Output:

```
out/
├── 01 - Statesboro Blues.flac … 04 - You Don’t Love Me.flac   <- disc 1 tracks, NO subfolder
└── Disc 02/01 - Hot ’Lanta.flac … 03 - Whipping Post.flac
```

Why: a regular audio folder converts as a **folder album batch** — each file is dispatched as an independent single-track job (`prepare_independent_single_file_album_batch_for_dispatch`, one `PipelineRequest`/`PreparedSource` per file; the conversion.log shows seven distinct job ids each planning `.track-0001`). `%DISC_FOLDER%` is gated by `source_has_proven_multi_disc_layout(source)` (stages.rs:15444), which is evaluated **per job**, where the source contains exactly one track:

- a disc 2 track proves a multi-disc layout by itself (disc number 2 > 1) → `Disc 02` created;
- a disc 1 track cannot (its only evidence is disc number 1, and the deliberate `album_metadata_disc_one_without_total_does_not_create_single_disc_folder` rule suppresses single-disc folders) → rendered into the album root.

**This specific repro is already fixed** (verified against the real album): the root cause was `materializer_single.rs` discarding the disc total — `extract_file_tag_metadata` never read `tag.disk_total()` and `derive_album_metadata` hardcoded `total_discs: metadata.disc_number.map(|_| 1)`. It now stores `disctotal` in the tag extras (which the stages hint machinery already consumes) and derives `total_discs` from it, so a lone disc 1 track proves the multi-disc layout via its `DISCTOTAL=2` tag. Additionally, `source_disc_folder_token` now names the output disc folder after the source disc directory when the disc number is corroborated by a path ancestor (`disc 1` in the source yields `disc 01`, digits normalized to two, prefix casing/separator preserved); tag-only disc numbers keep the `Disc NN` default. Tests: `disc_folder_token_preserves_source_disc_directory_naming_style` and `single_track_batch_job_with_disc_total_creates_disc_one_folder` in `title_extra_tests`, plus `derive_album_metadata_*` unit tests in `materializer_single.rs`.

One caveat for your triage: this fix relies on the tags carrying a disc total. A folder album batch whose files have `disc` numbers but **no** `DISCTOTAL` tag still splits per-track and disc 1 still can't prove multi-disc from a single-track source, even though the batch scope (sibling `disc N` directories under `source_grouping_root`) proves it. If you touch this area for failures 1–3, consider batch-scope disc-layout detection stamped into each request as the durable fix; the invariant is that within one album batch, either every track gets a disc folder or none does.

This failure is the same feature area as failures 1–3; triage them together.

## Related: CLI folder scan queues CUE sidecars alongside their split tracks

Observed while verifying the disc-folder fix (same album; each disc directory also contains two `.cue` files — one `{noncompliant}`, one `{Single Wave}` referencing an image that does not exist next to it). `tonepoet convert <album folder>` queues 11 items: 7 FLACs + 4 CUEs. The result:

- FLAC-only copy of the same tree: `7/7 succeeded, 0 failed`.
- Original tree with CUEs: `2/11 succeeded, 9 failed` — yet **all 7 output files publish correctly** (both per-disc conversion.logs report all tracks successful).

Two distinct problems: (a) **RESOLVED** — the CLI folder scan now routes through the same queue-expansion heuristics as the TUI (`plan_cli_convert_queue` in src/main.rs calls `expand_paths_to_audio_with_metadata` and applies `cue_sidecar_override_for_commit_path`); split-track folders no longer queue their CUEs, unsplit images queue the CUE and suppress the image, explicit CUE arguments still queue, and the same trees now convert 55/55 and 7/7 deterministically. One layering note for a future pass: the expansion logic still lives in `src/tui/browse.rs` and the CLI consumes it across the tui module boundary; extracting it to `src/convert/queue_expansion.rs` was deferred because it is coupled to Browse's `classify_file`/`EntryKind`. (b) queue item status accounting misreported when CUE items failed mid-batch (2 of 11 Completed although all 7 FLACs published). With (a) fixed the trigger is gone and it no longer manifests, but the accounting path was not itself fixed — a batch with a genuinely failing item may still miscount sibling items; re-verify if a mixed success/failure batch shows implausible summary numbers (summary printed from `completed_items()`/`failed_items()` in `run_convert`).

## Constraints

- Fix whichever side (test or implementation) matches the intended product behavior; note the decision per failure.
- Do not change the `pub(super)` visibility fix or the `use super::naming_template_tests::{template_request, template_source};` import — those are required for the test build to compile.
- All other 2523 lib tests pass; do not regress them.

## Relevant files

- `src/convert/pipeline/stages.rs` — failures 1–8 (tests and the template/planning/publish implementations)
- `src/tui/app.rs` — failure 9 (`OutputOptionsField` cycle + test)
