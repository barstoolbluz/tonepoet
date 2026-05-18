# Code task: Progress instrumentation foundation (v2 Milestone 1)

## Repo

https://github.com/barstoolbluz/tonepoet.git  
Branch: `main`, commit: `5c75034`

## Context

Read these documents for full background:

1. `docs/proposed-v2-roadmap-for-progress-system.txt` — the full v2 roadmap. This task implements **Milestone 1 only**.
2. `docs/progress_reporter_brief.md` — v1 architecture summary.
3. `docs/proposed-brief-for-improved-progress-reporting-subsystem.md` — UX requirements (v2 items are follow-up scope in that doc).

## What v1 already shipped (commit `5c75034`)

- `BroadcastReporter` in `src/convert/pipeline/reporter.rs`: maps `PipelineEvent` → `ProgressUpdate`, stage→phase windows, monotonic progress, 500ms throttle, terminal state preservation.
- Per-track `PipelineEvent::Progress` emission in `convert_tracks` (one site, `stages.rs:1522`).
- `RecordingReporter` preserved for tests.
- 603 tests green.

## What Milestone 1 delivers

A route-neutral progress instrumentation layer that sits between long-running operations and the existing pipeline reporter:

```
stage code / child process
        ↓
OperationProgressTracker
        ↓
PipelineEvent::Progress (existing variant)
        ↓
BroadcastReporter (existing, unchanged)
        ↓
ProgressUpdate → TUI
```

### 1. `OperationProgressTracker`

A struct that stage code creates at the start of a stage and calls into during work. It holds a reference to the reporter, the stage identity, and internal state (elapsed time, last progress, throttle state).

Methods (from the roadmap):
- `start_unit(ordinal, total, name)` — beginning a track/file within a stage
- `measured(progress, message)` — progress from a reliable source (tool output, byte counter)
- `estimated(progress, message)` — progress from duration/sample weighting
- `unknown_alive(message)` — no meaningful progress, but work is happening
- `finish_unit(ordinal, total, name)` — completed one unit
- `cancel_requested()` — immediate cancellation visibility

Each method emits `PipelineEvent::Progress` through the reporter, subject to throttling.

**Design note:** `PipelineReporter::emit` is `async fn`. The tracker's methods will need to be async, or the tracker can buffer events and flush them. The reasoning model should choose the approach that keeps call sites clean in the existing `convert_tracks` loop (which is already async).

### 2. Throttling

V1's `BroadcastReporter` already has 500ms throttle at the reporter level. Milestone 1 adds throttling at the tracker level (closer to the source) so high-frequency tool probes (Milestones 3-5) don't flood the reporter.

Rules from the roadmap:
- Send immediately when: stage changes, message changes materially, cancellation, failure, terminal state
- Otherwise send when: progress ≥ 0.5% change OR ≥ 500ms elapsed

### 3. Elapsed-time formatting

A helper that formats `Duration` as coarse human-readable text:
- Under threshold (e.g., 5s): omit
- Short: `48s`
- Medium: `2m 14s`
- Long: `1h 03m`

The tracker appends elapsed time to messages after the threshold.

### 4. `ProgressConfidence` enum

```rust
pub enum ProgressConfidence {
    Measured,    // parsed from tool output or byte/sample counters
    Estimated,   // duration/sample/item-weighted estimate
    Unknown,     // alive, but no meaningful denominator
}
```

And optionally `ProgressScope`:

```rust
pub enum ProgressScope {
    Overall,
    Stage,
    File,
    Track,
    Tool,
}
```

These are internal types for the progress subsystem. They do not change `PipelineEvent` or any locked contract.

### 5. Migration of existing emission site

The single `PipelineEvent::Progress` emission at `stages.rs:1522` should migrate to use the tracker. Currently:

```rust
reporter
    .emit(PipelineEvent::Progress {
        item_id: item_id.to_string(),
        stage: PipelineStage::Convert,
        phase_progress: phase_progress.clamp(0.0, 1.0),
        message: Some(message),
    })
    .await;
```

After migration, stage code calls the tracker instead of emitting raw events.

### 6. Tests

From the roadmap's test list, Milestone 1 requires:
- Throttling sends stage/message/terminal changes immediately
- Throttling coalesces high-frequency updates
- Elapsed time appears only after threshold
- Elapsed time formatting is coarse (`48s`, `2m 14s`, `1h 03m`)
- Confidence enum is typed, not inferred from strings
- `start_unit` / `finish_unit` emit correct messages
- `unknown_alive` does not advance progress
- `cancel_requested` emits immediately regardless of throttle

## What Milestone 1 does NOT deliver

- No tool-output parsing (ffmpeg/sox/archive) — Milestones 3-5
- No heartbeat for opaque operations — Milestone 2
- No ETA — Milestone 6
- No cancellation plumbing beyond the tracker method — Milestone 7
- No TUI display changes — Milestone 8

## File structure

New directory:
```
src/convert/pipeline/progress/
  mod.rs           — module root, re-exports
  operation.rs     — OperationProgressTracker
  throttle.rs      — throttling logic (may merge into operation.rs if small)
  confidence.rs    — ProgressConfidence, ProgressScope
  elapsed.rs       — elapsed-time formatting
```

Wire in `src/convert/pipeline/mod.rs` as `pub mod progress;`.

## Locked contracts (do not change)

- `PipelineEvent` enum
- `PipelineReporter` trait
- `ProgressUpdate` struct
- `ConversionStatus` enum

The tracker emits through `PipelineEvent::Progress` (the existing variant). No new event variants.

## Existing types the tracker should populate

`ConversionStatus::Processing` has `file_progress: Option<(u32, u32)>` for current-file/current-track progress. The tracker's `start_unit(ordinal, total, ...)` maps naturally to this field via the reporter.

## `#![forbid(unsafe_code)]`

All pipeline modules are under `#![forbid(unsafe_code)]`. `std::sync::Mutex` for internal state is fine.

## Quality requirements

The code must be:
- **Correct**: throttling rules match the spec, elapsed formatting is coarse, confidence is typed
- **Performant**: no allocation pressure on the hot path, throttle prevents flooding
- **Route-neutral**: no SACD/CUE/7z/FLAC-specific logic in the tracker
- **Idempotent**: safe to apply on a clean checkout of commit `5c75034`
- **Complete**: compiles and passes `cargo test --lib` (currently 603 tests, must not regress)

## Build & test

```bash
nix develop --extra-experimental-features 'nix-command flakes' --command cargo build
nix develop --extra-experimental-features 'nix-command flakes' --command cargo test --lib
```

## Deliverable

Generate downloadable, production-ready code files:
- New `src/convert/pipeline/progress/` module (all files)
- Patch or replacement for `src/convert/pipeline/mod.rs` (module wiring)
- Patch or replacement for `src/convert/pipeline/stages.rs` (migration of the existing emission site)
- Tests for the tracker, throttling, elapsed formatting, and confidence types
