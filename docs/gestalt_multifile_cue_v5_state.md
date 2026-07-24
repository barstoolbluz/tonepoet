# Gestalt: multi-FILE cue selection (v5) — the whole situation

Date: 2026-07-24. For a reasoning-model session. This is a **situation map, not a
prescriptive fix list.** Earlier corrective briefs over-specified the policy and
caused regressions (one told the implementer to "fail closed" broadly and turned
four previously-passing tests red). So this describes the *facts and the shape* of
the problem and leaves the policy and implementation to you. Where something is a
hard, test-verified contract it is labeled as such; everything else is yours to
reason about.

## 0. The one-paragraph gestalt

v5 delivers same-folder **cue selection**: when a folder holds more than one `.cue`,
classify/select/prompt instead of merging-or-hanging. It is **~90% correct and its
cue-disposition policy is coherent** — it compiles clean and passes the great
majority of the cue-related tests (≈35 in queue expansion alone, plus the
metadata-editor suite), including every "single bad cue" case and the main
real-world flows. The remaining problem is **6 localized loose ends** (7 failing tests). The
job is **surgical**: fix those six without disturbing the working policy. A sweeping
rework of the classification/fallback layer is the known failure mode and must be
avoided.

## 1. Baseline and how to read the evidence

- The tree is v5 as applied, plus three minimal applier compile-fixes (a test-only
  `tx()` helper in `single_image_metadata_editor_regression_tests`; two
  `#[cfg(test)]` markers on now-superseded middle-layer fns
  `command.rs::expand_..._with_grouping_decisions` and
  `keybindings.rs::collect_metadata_cue_surfaces_with_warnings`). It compiles with
  zero new cold-build warnings.
- Gate: `cargo test --workspace --no-fail-fast` → **4793 passed, 7 failed.** Those
  seven ARE the entire remaining problem.
- **The test suite is the ground truth.** Every currently-passing cue test is a
  contract; do not break one to fix another. It has been verified that v5 did NOT
  edit or weaken any pre-existing shipped test (all 8 audited bodies are
  byte-identical to the pre-v5 baseline), so the passing shipped behaviors are
  genuinely preserved and the failing shipped tests are clean regressions.

## 2. What v5 already gets right — DO NOT DISTURB

These behaviors are verified by currently-passing tests (and, where noted, by the
user on real trees). Treat them as invariants:

- **Single unusable cue → suppress the cue, keep the folder's audio**, for every
  reason: unparseable bytes (`…suppresses_unparseable_cue_and_keeps_audio`),
  ambiguous stem reference (`…suppresses_ambiguous…`), missing `INDEX 01`
  (`…suppresses_cue_missing_index01…`), non-increasing `INDEX`
  (`…suppresses_non_increasing…`), external/subdirectory/child-dir references
  (`…references_external_audio`, `…subdirectory_reference`,
  `…child_directory_split_source…`).
- **Unresolved-only cue (missing referenced file) → fall back to raw audio with a
  visible warning** (`unresolved_only_cue_falls_back_to_raw_audio_with_a_visible_warning`;
  metadata `metadata_unresolved_only_cue_degrades_to_plain_file_discovery`).
- **Same-image alternatives** (two cues resolving to the same image) → auto-select
  the unique exact-extension match, else prompt once
  (`folder_expansion_auto_selects_unique_exact_same_image_cue`,
  `same_image_alternatives_require_one_choice_and_queue_only_that_cue`,
  `metadata_same_image_alternatives_auto_select_the_unique_exact_cue`).
- **Multi-FILE album consolidation** — one cue → N distinct files opens as one album
  and converts once (`native_multi_file_cue_opens_as_one_album_from_folder_cue_or_member_image`,
  `four_file_native_multi_file_album_consolidates_persists_reopens_and_queues_once`).
  Uriah Heep (2 sides) and 80's Movie Hits (4 sides) work for the user.
- **Foxy core** — a folder with two cues resolving to the same `.wv` (one exact
  `WV.cue`, one same-stem `.cue` naming an absent `.wav`) auto-selects the exact
  cue, does not hang, and persists ALBUM (and other album-field) edits back to the
  selected sidecar (`foxy_alternative_cues_select_exact_sidecar_and_persist_save`,
  `foxy_folder_cue_and_image_routes_retain_sidecar_and_write_back_album_fields`).
  The user confirms Foxy works at runtime.
- **Metadata degrade** — an unusable or alien cue degrades to plain file/TOC
  discovery in the metadata editor (`mb_apply_unusable_cues_degrade_to_plain_file_toc_discovery`).

## 3. The hard-won cue-disposition rule (verified from the tests — the anti-trap)

This is the single most important fact, because getting it wrong is what broke a
prior attempt:

- A **single** cue that is unusable for ANY reason → **suppress the cue, keep the
  audio.** (Never fail closed on a single bad cue.)
- **Fail closed applies to exactly one situation**: a **merged multi-cue album
  group** (≥2 cues that group into one album) where a **member cannot be parsed** —
  because you cannot assemble a partial album. That is thread A below, and it is the
  only place fail-closed belongs.

A prior corrective made single bad cues fail closed; that turned
`suppresses_ambiguous / …child_directory_split_source / …missing_index01 /
…non_increasing` red (note: single-`unparseable` stayed correct even then —
it is the merged group, not the byte-level parse failure, that fails closed).
Do not repeat it.

## 4. The 6 loose ends (7 failing tests) — evidence, for you to resolve

### Shipped-contract divergences (v5 changed a v4 behavior; test bodies unchanged)

- **A — merged album group with an unparseable member is not failing closed.**
  `folder_expansion_fails_closed_when_merged_cue_group_cannot_be_parsed`: folder
  has `side_a.cue` (valid) + `side_b.cue` (garbage bytes), referencing distinct
  images. Contract: empty paths + a parse/analyze/decode error. v5 instead queues
  `side_a.cue` + raw `side_b.flac`.
- **B — the `EmbeddedOnly` cue-sidecar override is dropped for sibling audio of an
  error/artifact cue.** `expand_paths_to_audio_marks_sibling_audio_when_nonexplicit_cue_errors`
  (queue) and `cli_convert_queue_planning_tests::cue_artifact_audio_gets_embedded_only_override`
  (CLI planner) are the same behavior: audio queued beside such a cue must carry
  `Some(EmbeddedOnly)`; v5 yields `None`.
- **C — a split-source cue no longer wins over a 1-track artifact cue on the same
  image.** `split_source_cue_suppresses_audio_shared_with_artifact_cue`: `album.cue`
  (2 tracks, split source) + `album-index.cue` (1 track, artifact) + `album.flac`.
  Contract: queue `album.cue`, suppress `album-index.cue` and `album.flac`. v5's
  role-neutral selection stopped auto-picking the split source.

For A/B/C the failing test bodies are the v4 contracts (unchanged). The gate
requires them green. If you believe any of these shipped contracts is actually
*wrong* and v5's new behavior is preferable, **stop and say so in your report** —
do not silently edit a shipped test.

### Unfinished intent (v5's own new tests; it couldn't run them)

- **D — the queue does not emit a synthetic-cue-album artifact for a
  merged/selected MULTI-FILE cue album.** `editor_and_queue_select_the_sole_viable_multi_file_cue`:
  (phase 1) two multi-FILE cues with a shared title prefix should merge into one
  synthetic album; (phase 2) after a member image is deleted, the sole viable
  multi-FILE cue should still produce one album. Both expect `queue.paths.len()==1`
  and `is_synthetic_cue_album_artifact(queue.paths[0])`. v5's `queue.paths[0]` is
  not that artifact. (Note: the metadata-editor consolidation and the single 4-file
  cue queue both pass — this is a specific queue sub-case.)
- **E — wrong status wording for unresolved-only + no audio.**
  `metadata_unresolved_only_cue_without_audio_surfaces_the_fallback_failure` expects
  the status to contain "no CUE", "ordinary file/TOC discovery", and "no supported
  audio files were found".
- **F — a selected single sidecar cue's `CATALOG` is not surfaced as an editable
  `CATALOGNUMBER` row.** `foxy_exact_selection_persists_through_real_wavpack_save_worker`:
  the exact cue contains `CATALOG 25AP-1115`; the test selects it (works), edits
  ALBUM (works), then edits CATALOGNUMBER and panics "missing CATALOGNUMBER row"
  before it ever reaches the save/persist assertions. So Foxy's runtime fix is
  intact; this is purely a missing editable row. The builder
  `build_metadata_editor_for_cue_surfaces_with_policy` already has CATALOGNUMBER
  surfacing (keybindings.rs ~12027/12146) gated on the surface's parsed `catalog`;
  for the selected single sidecar surface that gate is not producing the row.

## 5. Constraints (hard)

- **Surgical, minimal diff.** Do not rework the classification/selection/fallback
  policy — it is ~correct (see §2/§3). Sprawling changes will be rejected.
- Do not edit or weaken the ~35 passing cue tests, nor the A/B/C shipped test
  assertions. You may adjust D/E/F (your own tests) only if an assertion encodes the
  wrong expectation, and then explain why.
- **Do not make single bad cues fail closed** (§3).
- Out of scope, unchanged: `src/convert/pipeline/**`, `db.rs`, crash-recovery/
  journal, the folder-name sanitizers, and the deferred log-file/DR-TOC-inference
  matching idea.
- Runtime reality: Foxy, Uriah Heep, and 80's Movie Hits already work for the user;
  do not regress the runtime while turning tests green.

## 6. Acceptance gate

`cargo test --workspace --no-fail-fast`: all 56 result lines report `0 failed`; the
seven named tests pass; the ~35 currently-passing cue tests stay green; zero new
cold-build warnings; no new production `unwrap`/`expect`/`panic` on user-controlled
cue data. Report your seams: for each of A–F, what you changed and why, and any
shipped-contract you think is wrong (flagged, not edited).

## 7. The seven failing tests (verbatim)

- `convert::queue_expansion::tests::folder_expansion_fails_closed_when_merged_cue_group_cannot_be_parsed`
- `convert::queue_expansion::tests::expand_paths_to_audio_marks_sibling_audio_when_nonexplicit_cue_errors`
- `convert::queue_expansion::tests::split_source_cue_suppresses_audio_shared_with_artifact_cue`
- `cli_convert_queue_planning_tests::cue_artifact_audio_gets_embedded_only_override`
- `tui::keybindings::single_image_metadata_editor_regression_tests::editor_and_queue_select_the_sole_viable_multi_file_cue`
- `tui::keybindings::single_image_metadata_editor_regression_tests::metadata_unresolved_only_cue_without_audio_surfaces_the_fallback_failure`
- `tui::keybindings::single_image_metadata_editor_regression_tests::foxy_exact_selection_persists_through_real_wavpack_save_worker`
