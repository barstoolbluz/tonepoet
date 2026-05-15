# Session handoff — tonepoet conversion pipeline rebuild

Read this first, then read the execution plan. Do not re-plan or
re-audit the plan — it is settled.

## Where we are

Project: `tonepoet`, repo `/home/daedalus/dev/tonepoet`, branch `main`.

We are executing a multi-PR rebuild of the conversion pipeline.

**Execution source of truth (authoritative — follow faithfully):**
`docs/phase0_sequencing_plan_hardened_ready_for_execution.md`
(PR 1–10, 19 global invariants). The older
`docs/phase0_sequencing_plan.md` is superseded.

Memory artifact: `project_pipeline_rebuild_plan.md`.

Build/test — the project requires nix:
```
nix develop --extra-experimental-features 'nix-command flakes' --command cargo build
nix develop --extra-experimental-features 'nix-command flakes' --command cargo test --lib
```

## PR 1 status: IMPLEMENTED BUT NOT COMPLETE, NOT COMMITTED

PR 1 ("Contracts") was implemented. A self-audit caught two skipped
plan requirements (findings 1–2 below) plus a deviation needing a
decision (finding 3). **Do not commit PR 1 until these are resolved.**

### FIRST: do your own full PR 1 audit — do not trust the list below

The prior session repeatedly missed things, including in its own
audits. The findings below are a **non-exhaustive starting point**,
not a verified-complete list. Before fixing anything:

1. Open `docs/phase0_sequencing_plan_hardened_ready_for_execution.md`,
   the "PR 1 - Contracts" section.
2. Walk **every** sentence of that section and **every bullet** of
   the "PR 1 exit condition" list. For each, verify against the
   actual code (grep/read — not memory, not this handoff) whether it
   is satisfied.
3. Produce your own findings list. Findings 1–5 below should appear
   in it; if they don't, or if you find more, trust your own audit.
4. Only then fix. The prior session's "PR 1 complete" claim was
   false — assume nothing here is complete until you have verified
   it yourself.

### What is done

- New module `src/convert/pipeline/` — files `mod.rs`, `types.rs`,
  `tool.rs`, `errors.rs`, `reporter.rs`, `stages.rs`. Ships all
  contract types, error types, the `Materializer`/`ToolRunner`/
  `PipelineReporter` traits, every stage-function signature with
  compiling no-panic stub bodies, `StubToolRunner`, `RecordingReporter`,
  and the real `aggregate_album_outcome` + `map_album_outcome`.
  Module is `#![forbid(unsafe_code)]`.
- `ConversionStatus` (in `src/convert/queue.rs`) extended: `Completed`
  and `Failed` gained `log_path`; new terminal `Partial { output_path,
  successful, failed, log_path }`. ~17 ripple sites fixed across
  `processor.rs`, `convert/mod.rs`, `main.rs`, and TUI files.
- `Cargo.toml`: added `tokio-util` (feature `rt`) and `async-trait`.
- Tests: 17 pipeline tests; full lib suite 527/527 green; build clean.
- During the `ConversionStatus` ripple fix, `Partial` display arms
  were already added to several match sites: `draw_overlays.rs`,
  `draw_queue.rs`, `event_loop.rs`, `main.rs`, `convert/mod.rs`. So
  `Partial` is already partly wired for display — verify those arms
  are correct as part of your audit.

### Git working-tree state — READ CAREFULLY before committing

PR 1 is uncommitted, and the working tree is **not clean**. Three
categories of change are mixed together:

**(A) Cleanly PR 1 — safe to `git add` whole-file:**
`Cargo.toml`, `src/convert/pipeline/` (new directory),
`src/convert/queue.rs`, `src/convert/mod.rs`,
`src/convert/processor.rs`, `src/main.rs`,
`src/tui/draw_overlays.rs`, `src/tui/draw_queue.rs`.
These were untouched before this work; their entire diff is PR 1.

**(B) PR 1 edits MIXED with pre-existing uncommitted changes —
stage hunks, do NOT `git add` whole-file:**
`src/tui/context_menu.rs`, `src/tui/event_loop.rs`,
`src/tui/keybindings.rs`. These were already modified *before* PR 1
started; PR 1 then added `ConversionStatus` ripple edits on top. Use
`git diff <file>` and stage only the `ConversionStatus`-related
hunks (the `log_path` / `Partial` pattern fixes) — e.g. `git add -p`.
Leave the pre-existing hunks unstaged.

**(C) Dirty but NOT PR 1 — do not touch:**
`src/tui/command.rs`, `src/tui/message.rs` (pre-existing edits,
PR 1 never touched them); plus noise — `target/` build artifacts,
`dst_port_files.zip`, a screenshot file, `.claude/`.

Bottom line: do not blindly `git add -A`. Stage category (A) whole,
category (B) by hunk, and exclude (C) entirely. Verify the staged
diff with `git diff --cached` before committing PR 1.

### PR 1 OUTSTANDING — fix before committing

**Finding 1 (must fix) — queue semantics for `Partial` not done.**
The plan ("Queue semantics updated in PR 1") requires: `Partial` is
terminal for `is_finished`; counted separately from completed and
failed; retry behavior explicit and test-locked. None of this was
done. In `src/convert/queue.rs`:
- `is_finished` (~line 215) — add `Partial`.
- the `matches!` at ~line 300-302 — add `Partial`.
- the count helpers that filter on `Completed`/`Failed` (~lines
  345 and 352 — confirm the actual fn names by reading) — decide
  where `Partial` counts (plan says "counted separately"); likely a
  new `partial_count`.
- cleanup `retain` (~371) — decide `Partial` retention.
- `can_retry` (~222) — make `Partial`'s retry behavior explicit and
  add a test that locks it.
- Add a test proving `Partial` is terminal for queue accounting
  (PR 1 exit condition).

**Finding 2 (must fix) — `ConversionItem.pipeline_request` missing.**
The plan: "`ConversionItem` gets an optional `pipeline_request:
Option<PipelineRequest>` during migration." Add that field to
`ConversionItem` in `src/convert/queue.rs` (with `#[serde(default)]`
so existing persisted queues still deserialize). Legacy fields stay.

**Finding 3 (needs USER DECISION before acting).**
`aggregate_album_outcome` cannot represent a non-blocking failed
stage. Plan exit #7 and PR 6 both want an optional-stage failure to
be a *failed* `StageRecord` that does not block. The current impl
blocks on any `StageOutcome::Failed`. The plan's fixed signature
`aggregate_album_outcome(tracks, stages, policy)` carries no
`StagePolicy`, so it genuinely cannot tell required from optional.
Two options — ASK THE USER which:
  (a) Accept "the orchestrator downgrades optional-stage failures to
      `StageOutcome::Skipped` before aggregation; the real error text
      goes to the durable log." (Current code comment already says
      this; just make it an agreed, documented deviation.)
  (b) Change the contract — e.g. add a non-blocking failed
      representation, or pass stage requirement into aggregation.
      This touches PR 1's contract surface, so it needs an explicit
      plan deviation.

**Finding 4 (minor) — note only.** `StagingDir` has an extra private
`armed: bool` field beyond the plan's literal `{ pub root, pub
job_id }`; required for the RAII `Drop` the plan describes. Acceptable;
mention it in the PR description as a deliberate minor deviation.

**Finding 5 (minor) — test gaps.** Plan wants `SecretString`
redaction tested in reporter messages and durable logs (only Debug/
Display/transcript/command-record are currently tested). Add coverage
or note the gap.

### After PR 1 findings are fixed

1. Re-run: `cargo build` + `cargo test --lib` — both must be green.
2. Re-check **every** PR 1 exit-condition bullet in the plan literally,
   one by one, before declaring PR 1 done. Do not claim "done" from
   memory.
3. Commit PR 1 (the user commits only when satisfied — confirm first).
4. Proceed to PR 2 — "Real `ToolRunner`" — per the plan.

## Working discipline (important — prior session kept slipping)

- The hardened plan is authoritative. For each PR, before claiming it
  is done, walk its **exit-condition list bullet by bullet** and
  verify each against the actual code (grep/read — not memory).
- Do not report "complete" or "exit conditions met" unless every
  bullet is verified. The prior session falsely claimed PR 1 done.
- When the plan is internally ambiguous or self-contradictory (e.g.
  finding 3), STOP and ask the user — do not silently pick an
  interpretation and bury it in a comment.
- Audit by reading the code, not by recalling what you intended to
  write.
- Do not re-open or re-audit the plan itself — it is settled.
- Keep PR scope to exactly what the plan's PR says; no extras.

## Quick file map (PR 1)

- `src/convert/pipeline/mod.rs` — module root, `#![forbid(unsafe_code)]`,
  re-exports, 17 tests.
- `src/convert/pipeline/types.rs` — request/identity/artifact/outcome
  types, `SecretString`, `StagingDir`.
- `src/convert/pipeline/tool.rs` — `ToolBinary`, `ToolCommand`,
  `ToolOutput`, `CommandRecord`, `ToolRunner`, `StubToolRunner`.
- `src/convert/pipeline/errors.rs` — all 14 error enums.
- `src/convert/pipeline/reporter.rs` — `PipelineEvent`,
  `PipelineReporter`, `RecordingReporter`.
- `src/convert/pipeline/stages.rs` — `Materializer` trait, all stage
  function signatures + stubs, real `aggregate_album_outcome` +
  `map_album_outcome`.
- `src/convert/queue.rs` — `ConversionStatus` extended here; the file
  needing finding-1 + finding-2 fixes.
