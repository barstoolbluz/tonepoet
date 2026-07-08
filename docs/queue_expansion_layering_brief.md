# Brief: give queue expansion and file classification a proper home

Date: 2026-07-07

## What's wrong

Nothing is broken — this is an architecture correction. The queue-expansion
heuristics (CUE suppression/promotion, disc-root preservation, deterministic
dedup) and the file classification they depend on live inside
`src/tui/browse.rs` (17,781 lines), and are now consumed by non-TUI code:

- `src/main.rs:928` — `plan_cli_convert_queue()` calls
  `tonepoet::tui::browse::expand_paths_to_audio_with_metadata` so the CLI
  `convert` command applies the same CUE semantics as the TUI. This fixed a
  real bug (the CLI's raw folder walk queued CUE sheets that then failed as
  decomposition sources), but it made a binary depend on a UI module for
  domain logic.
- `src/tui/convert_actions.rs:541` — `cue_sidecar_override_for_commit_path`
  was made `pub` so the CLI and the TUI commit path share the CUE-artifact →
  `CueSidecarPolicy::EmbeddedOnly` mapping.

Deciding "what is this file and how should it be queued" is conversion-domain
knowledge, not presentation. The dependency direction should be
`tui → convert` and `main → convert`, never `main → tui` for queueing
semantics.

## Current shape (all in src/tui/browse.rs unless noted)

Queue expansion:

- `QueueExpansionResult` (line 10671) — queued paths + `cue_artifact_audio`
  metadata consumed downstream for sidecar-CUE policy.
- `expand_paths_to_audio_with_metadata` (10774) — the public entry; explicit
  file inputs keep explicit semantics, directories expand recursively
  (symlinks skipped).
- `expand_paths_to_audio_with_preserved_disc_roots` (10782) — variant used by
  Browse multi-selection to keep disc-source directories opaque.
- `QueueExpansionPlan` (10807) + `into_queue_paths` — collect-then-decide
  design: all candidates are gathered before any CUE decision so late
  discoveries can suppress earlier ones (see the doc comment on the struct).
- `CueQueueDecision` (11022), `cue_queue_decision_for_path` (11040),
  `analyze_cue_for_queue` (11128) — the CUE heuristics: split-source CUE →
  queue CUE / suppress referenced audio; already-split → suppress CUE as
  metadata artifact; unresolvable CUE → suppress and mark sibling audio via
  `mark_sibling_audio_as_cue_artifacts` (10954).
- Helpers: `queue_path_key` (11329), `push_unique_path_with_keys` (11341),
  `path_key_is_under_any_root` (10950), `collect_queue_candidates` /
  `_recursive` (10976/10986), `is_queueable_file` (11586),
  `is_cue_sheet_path` (11534).

File classification (the coupling that blocked a quick extraction):

- `EntryKind` (1149) — used pervasively as Browse's presentation/entry model
  AND as the classification result the expansion consumes.
- `classify_file` (11616, `pub(super)`) — extension-based classification;
  also consulted by `is_queueable_file`, which additionally gates ISOs by
  disc-type probes (`crate::tui::sacd::is_sacd_iso`,
  `crate::disc::dvda_utils::is_dvda_iso`, `crate::disc::dvdv_utils`).

Consumers outside browse.rs (occurrence counts of the expansion/classification
symbols): `src/tui/command.rs` (18 — including the async Browse Convert
folder-expansion worker, which classifies files during its walk),
`src/tui/keybindings.rs` (8), `src/tui/event_loop.rs` (5), `src/tui/app.rs`
(4), `src/tui/context_menu.rs` (1), `src/main.rs` (1).

## Goal

Make the elegant version true:

1. A conversion-domain module (suggested: `src/convert/queue_expansion.rs`,
   and if you split classification separately, something like
   `src/convert/classify.rs`) owns file classification and queue expansion.
   You decide the exact module boundaries — in particular whether Browse's
   `EntryKind` becomes a presentation view over a domain classification enum,
   or whether `EntryKind` itself moves down and Browse re-exports it. Note
   `EntryKind` currently mixes domain variants (`AudioFile`, `Archive`,
   `BlurayDir`) with presentation-only ones (`ParentDir`) — that asymmetry is
   the crux of the design decision.
2. `src/main.rs` and `src/tui/convert_actions.rs` consume the domain module;
   no non-TUI code imports from `tonepoet::tui::*` for queueing semantics.
   `cue_sidecar_override_for_commit_path` (or its successor) belongs with the
   expansion result it interprets.
3. Browse keeps working identically, via re-exports or updated imports —
   your choice, but existing call sites in command.rs / keybindings.rs /
   event_loop.rs / app.rs / context_menu.rs should not need semantic changes.

## Hard constraints

- **Zero behavior change.** This is a relocation, not a redesign. Every
  heuristic decision must be preserved exactly, including
  `queue_path_key`'s canonicalize-with-fallback identity (11329) and the
  symlink-skipping walk policy in `collect_queue_candidates_recursive`.
- **Tests move with the code and must keep passing unmodified in substance.**
  browse.rs has 166 `#[test]`s total; the `browse::` lib-test filter currently
  passes 228 tests. The expansion/CUE tests live in `mod tests` (11723) and
  the classification tests in `folder_content_classification_tests` (16466).
  The CLI planner tests are `cli_convert_queue_planning_tests` in
  `src/main.rs` (6 tests, model the real-world failing shapes — keep them
  green, they are the acceptance tests for the recent CLI fix).
- Deterministic output (sorted, deduplicated) is contract, not accident.
- 9 known pre-existing lib-test failures are documented in
  `docs/pre_existing_test_failures_triage_brief.md` — unrelated; do not
  regress anything else.
- The sandbox you run in cannot compile; the applier will fix compile errors,
  so favor mechanical-verifiability: prefer moving code verbatim with
  adjusted paths over rewriting bodies.

## Non-goals

- Do not change the CUE heuristics, the expansion order, or the CLI planner's
  observable behavior (Dreams box set: 55 queued / 55 succeeded; Fillmore:
  7 queued / 7 succeeded — both deterministic across runs).
- Do not touch the conversion pipeline (`src/convert/pipeline/`).
- The Browse UI's use of `EntryKind` for rendering/sorting/filtering is fine
  as-is; only the ownership/direction is at issue.

## Files in this bundle

- `docs/queue_expansion_layering_brief.md` — this brief
- `src/tui/browse.rs` — the code to relocate + its tests
- `src/main.rs` — CLI consumer (`plan_cli_convert_queue`, planner tests)
- `src/tui/convert_actions.rs` — commit-path consumer + shared override helper
- `src/tui/command.rs` — heaviest TUI consumer (async expansion worker)
- `src/tui/app.rs`, `src/tui/event_loop.rs`, `src/tui/context_menu.rs`,
  `src/tui/keybindings.rs` — remaining TUI consumers
- `src/tui/mod.rs`, `src/lib.rs`, `src/convert/mod.rs` — module wiring
- `src/convert/formats.rs` — existing conversion-domain neighbors
  (`FormatDetector`, `AudioFormat`) the classification layer should sit beside
