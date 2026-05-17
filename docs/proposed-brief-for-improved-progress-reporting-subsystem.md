# Agent task: Create expanded brief for rich pipeline progress UX

Read `progress_reporter_brief.md` and the follow-up UX feedback. Do not implement code yet.

Create a new markdown brief named:

`progress_reporter_rich_ux_brief.md`

The new brief should expand the original “Pipeline → TUI progress reporting” work into a richer, production-ready progress UX plan for Tonepoet.

The brief must give a future coding agent enough context to implement the work from a clean checkout of:

- repo: `https://github.com/barstoolbluz/tonepoet.git`
- branch: `main`
- commit: `afc87b1`

## Product direction

Optimize for user trust, not prettier percentages.

A user should always be able to answer:

- Is the job alive?
- What is it doing right now?
- Which file, archive member, track, or stage is blocking?
- How far did it get before failure or cancellation?
- Is the displayed progress measured, estimated, or unavailable?

The original bug showed a frozen `0% Extracting` display during long conversion work. The expanded design should prevent that class of UX failure across all conversion routes, not only SACD.

## Required code inspection

Before writing the new brief, inspect the current repo and summarize the relevant findings.

Inspect at least:

- `src/convert/processor.rs`
- `src/convert/pipeline/reporter.rs`
- `src/convert/pipeline/stages.rs`
- any definitions for:
  - `PipelineEvent`
  - `PipelineReporter`
  - `PipelineStage`
  - `StageRecord`
  - `ProgressUpdate`
  - `ConversionStatus`
  - `ConversionPhase`
  - any existing `file_progress` field or equivalent
  - any child-process runners for ffmpeg, sox, archive extraction, tagging, ReplayGain, or feature extraction

Identify which recommendations can fit the current type contracts and which would require type or API changes.

Prefer designs that preserve these locked contracts from the original brief:

- `PipelineEvent`
- `PipelineReporter`
- `ProgressUpdate`
- `ConversionStatus`

Do not recommend changing those contracts unless the brief marks that work as a separate future option.

## Required output

Write one markdown file:

`progress_reporter_rich_ux_brief.md`

It should contain:

1. Problem statement
2. Product goal
3. Non-goals
4. Current architecture summary
5. Current event flow and gaps
6. Proposed architecture
7. Route-neutral progress model
8. Event semantics
9. Stage-to-phase mapping
10. Message strategy
11. Progress calculation strategy
12. Duration/sample weighting strategy
13. Unknown-progress strategy
14. Elapsed-time and ETA strategy
15. Failure, cancellation, and partial-success semantics
16. Monotonicity and rounding rules
17. Throttling/coalescing rules
18. Optional tool-output parsing strategy
19. File-by-file implementation plan
20. Test plan
21. Build/test commands
22. Acceptance criteria
23. Follow-up work list

Include code snippets or pseudocode for the hardest parts.

## Required separation of work

Divide recommendations into two groups:

### First production patch

This group should contain improvements that are realistic, low-risk, and likely to fit the current contracts.

Strong candidates:

- `BroadcastReporter`
- stage-to-phase mapping
- stage-window progress calculation
- monotonic progress state
- terminal progress semantics
- start-of-work `PipelineEvent::Progress`
- completion `PipelineEvent::Progress`
- useful progress messages
- per-track/per-file progress where the pipeline already has enough data
- duration/sample weighting if that data already exists
- simple throttling if event frequency increases
- tests for the reporter and event emission points

### Follow-up progress subsystem

This group should contain larger or more route/tool-specific work.

Candidates:

- ffmpeg stderr parsing
- sox progress parsing
- archive byte/file progress parsing
- heartbeat events during opaque child processes
- conservative ETA
- richer measured/estimated/unknown confidence model
- immediate cancellation progress while a child process is still terminating
- UI changes beyond the existing `ProgressUpdate` / `ConversionStatus` contracts

## Architecture guidance

Avoid hardcoding SACD-specific, CUE-specific, FLAC-specific, or 7z-specific behavior inside `BroadcastReporter`.

`BroadcastReporter` should handle route-neutral responsibilities:

- map `PipelineStage` to `ConversionPhase`
- map stage windows to total progress
- convert `PipelineEvent` into `ProgressUpdate`
- preserve last-known progress for terminal failed/cancelled states
- keep progress monotonic per item
- round user-visible progress
- optionally coalesce frequent updates
- ignore broadcast send failures

Pipeline stages and runners should emit facts:

- current file or track name
- ordinal and total
- estimated duration or sample count, when available
- measured tool progress, when available
- skipped-stage details
- failure or partial-success details
- messages that explain current work

This shape should benefit:

- SACD conversion
- 7z archive extraction plus conversion
- CUE conversion
- batches of FLAC files
- future conversion routes

## Required UX behaviors

### 1. Starting state

The first user-visible update should not imply that extraction has already started if setup is still happening.

Use messages such as:

- `Starting conversion…`
- `Preparing source…`
- `Extracting source…`

### 2. Start-of-work events

Emit `PipelineEvent::Progress` before expensive work starts, not only after it finishes.

Examples:

- `Extracting archive`
- `Reading CUE sheet`
- `Preparing DSD source`
- `Converting track 1 of 2: So What`
- `Writing tags`
- `Calculating ReplayGain`
- `Publishing files`
- `Writing conversion log`

### 3. Completion events

After each meaningful unit completes, emit a short completion message.

Examples:

- `Finished track 1 of 2`
- `ReplayGain complete`
- `Metadata written`
- `Archive extracted`

### 4. Track/file identity

Use available names in messages.

Prefer:

- `Converting track 1 of 2: So What`

over:

- `Converting track 1 of 2`

For archives or file batches, prefer:

- `Extracting 03 - Blue in Green.flac`
- `Converting 03 - Blue in Green.flac`

over generic messages.

### 5. Weighted Convert progress

Do not use track count as the only model when better data already exists.

Preferred order:

1. completed samples / total samples
2. completed duration / total duration
3. completed items / total items as fallback

Keep the original Convert window unless code inspection finds a strong reason to propose a later revision:

- `Convert = 20–80%`

If duration/sample metadata does not already exist, the brief should say where it could come from and whether that belongs in the first patch or follow-up work.

### 6. Unknown-but-active state

When a child process cannot report meaningful progress, say so honestly.

Examples:

- `Converting track 1 of 1 · still running`
- `Converting track 1 of 1 · progress unavailable from sox`

Do not invent fine-grained percentages for opaque work.

Optional heartbeat events may be proposed as follow-up work. If proposed, require throttling.

### 7. Elapsed time

Surface elapsed time once work has been running long enough to matter.

Examples:

- `Converting track 1 of 2: So What · elapsed 2m 14s`
- `Extracting archive · elapsed 48s`

The brief should specify where elapsed time should be measured:

- in the reporter,
- in stage helpers,
- or in a route-neutral progress helper.

Prefer the location that keeps route-specific code small and avoids changing locked contracts.

### 8. Conservative ETA

ETA is optional. If proposed, mark it as follow-up unless the existing code already provides enough signal.

Rules:

- do not show ETA during the first track unless tool-level progress exists
- use duration/sample-weighted history when available
- round heavily: `about 7m remaining`
- hide ETA after failures, skips, cancellation, or denominator changes
- never display fake-looking exact times such as `6m 43s remaining`

### 9. Failure and cancellation

Do not map failed or cancelled jobs to `100%`.

Rules:

- Completed: `100%`
- Partial: `100%`, with a message explaining the partial result
- Failed: preserve last-known progress
- Cancelled: preserve last-known progress
- Queued / Paused / NotConfigured: `0%`

Examples:

- `Failed during extraction at 12%`
- `Cancelled at 37%`
- `Partial: 8 of 9 tracks converted`

### 10. Partial success

Make partial results visible while the job continues.

Examples:

- `Track 4 failed; converting track 5 of 9`
- `File 3 failed; continuing with file 4 of 9`

Final message example:

- `Partial: 8 of 9 tracks converted`

The brief should inspect current error-handling behavior before recommending the exact emission points.

### 11. Skipped optional stages

Skipped optional stages should be visible but not alarming.

Examples:

- `ReplayGain skipped`
- `Feature extraction skipped`

The next active stage should replace the skipped-stage message.

### 12. Monotonic progress

The reporter should not send lower progress for the same item unless the job restarts or returns to a queued/pre-run state.

This prevents visible regressions such as:

- `80% Converting`
- `75% Converting`

The brief should describe the required per-item state and the synchronization primitive needed to satisfy `Send + Sync`.

### 13. Avoid false precision

Round user-visible progress to whole percentages or at most one decimal place.

Prefer:

- `43% Converting`

over:

- `43.772196% Converting`

The brief should identify whether rounding belongs in `BroadcastReporter`, `ConversionStatus`, or the TUI.

### 14. Tool-output progress

Consider optional parsers only where cheap and reliable.

Potential examples:

- ffmpeg: parse `time=...`
- sox: parse progress/position only if available
- archive extraction: parse file or byte progress only if available

Do not make tool parsing mandatory for the first production patch unless code inspection shows an existing parser or wrapper that makes it straightforward.

If proposed, isolate tool parsing behind route-neutral helpers and throttle emitted events.

### 15. Throttling/coalescing

If tool-level progress or heartbeat progress is added, coalesce frequent events.

Suggested send rules:

Send immediately if:

- stage changed
- terminal status arrived
- failure/cancellation arrived
- message changed in a non-heartbeat way

Otherwise send only if:

- progress advanced by at least `0.5%`, or
- at least `500ms` elapsed since the previous send for that item

Repeated elapsed-time-only messages should not flood the broadcast channel.

## Existing stage-window mapping

Start from the mapping in the original brief:

| PipelineStage | ConversionPhase | Progress window |
|---|---|---|
| Materialize | Extracting | 0–15% |
| PlanOutputs | Analyzing | 15–20% |
| Convert | Converting | 20–80% |
| Merge | Converting | 80–85% |
| Metadata | Tagging | 85–90% |
| ReplayGain | PostProcessing | 90–93% |
| Features | PostProcessing | 93–95% |
| Publish | Finalizing | 95–98% |
| DurableLog | Finalizing | 98–100% |

The expanded brief may propose substage progress inside those windows, but it should not revise the top-level windows in the first patch unless code inspection reveals a clear reason.

## Questions the new brief must answer

- Where should `BroadcastReporter` live?
- Should `BroadcastReporter` keep mutable per-item state?
- What synchronization primitive should it use?
- Where should elapsed time be measured?
- Where should throttling live: reporter, stage helpers, or tool parsers?
- Can the current `ConversionStatus::Processing` carry useful messages?
- Does current code support `file_progress` without changing contracts?
- What data already exists for track duration or sample weighting?
- What data is missing?
- Which changes belong in the first production patch?
- Which changes belong in follow-up work?
- Which tests should fail before the patch and pass after it?

## Required pseudocode/code snippets

Include snippets or pseudocode for:

1. `BroadcastReporter` struct and state
2. stage-window mapping
3. `PipelineEvent` to `ProgressUpdate` conversion
4. monotonic progress handling
5. terminal status handling
6. duration/sample-weighted Convert progress
7. start-of-track and finish-of-track event emission
8. skipped-stage event emission
9. throttling/coalescing
10. representative unit tests

## Test plan requirements

The new brief should require tests for:

- stage start maps to the correct phase and start percentage
- stage finish maps to the correct phase and end percentage
- Convert progress maps into the `20–80%` window
- progress does not move backward
- Failed preserves last-known progress
- Cancelled preserves last-known progress
- Completed reaches `100%`
- Partial reaches `100%` with a useful message, if current contracts allow it
- send failures from the broadcast channel do not fail the pipeline
- start-of-work events occur before long conversion calls
- completion events occur after unit completion
- duration/sample weighting falls back to item count when metadata is missing
- throttling permits stage/message/terminal changes immediately

## Build and test commands

Use the original project commands:

```bash
nix develop --extra-experimental-features 'nix-command flakes' --command cargo build
nix develop --extra-experimental-features 'nix-command flakes' --command cargo test --lib
