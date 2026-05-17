# Code task: Implement rich pipeline progress reporting

## Repo

https://github.com/barstoolbluz/tonepoet.git  
Branch: `main`, commit: `398be2c`

## Instructions

Read these documents in order:

1. `docs/progress_reporter_brief.md` — factual summary of the current architecture, event types, call sites, and stage→phase mapping
2. `docs/proposed-brief-for-improved-progress-reporting-subsystem.md` — comprehensive UX requirements and design constraints

Then inspect the codebase at the referenced commit. The proposed brief lists specific files and types to inspect.

Then generate production-ready, downloadable code files that implement the **first production patch** as defined in the proposed brief. The follow-up work items are out of scope for this task.

## Key context from recent debugging

A real-world SACD conversion (Miles Davis, 2 tracks DSD64) showed:
- Track 1 sox DSD→PCM took **162 seconds** — user saw frozen "0% Extracting" the entire time
- The pipeline emits `StageStarted`/`StageFinished` events but `PipelineEvent::Progress` is **defined but never emitted** anywhere in the orchestrator
- All three routing paths (7z, CUE, SACD) use `RecordingReporter` which captures events in memory but never forwards to the TUI
- The TUI broadcast channel (`broadcast::Sender<ProgressUpdate>`) is available at all three call sites but unused by the reporter

## What already exists

- `PipelineEvent` enum with `StageStarted`, `StageFinished`, `Progress`, `Terminal` variants
- `PipelineReporter` trait: `async fn emit(&self, event: PipelineEvent)`, requires `Send + Sync`
- `RecordingReporter` for tests (keep it — don't delete)
- `ProgressUpdate` struct: `{ item_id, progress: f32, status: ConversionStatus }`
- `ConversionStatus::Processing` with `progress`, `message`, `file_progress`, `phase`, `phase_progress` fields
- `ConversionPhase` enum with weighted progress windows
- `broadcast::Sender<ProgressUpdate>` available at all three routing call sites in `processor.rs`

## Locked contracts (do not change)

- `PipelineEvent` enum
- `PipelineReporter` trait  
- `ProgressUpdate` struct
- `ConversionStatus` enum

## Quality requirements

The code must be:
- **Correct**: accurate stage→phase mapping, monotonic progress, proper terminal states
- **Performant**: `broadcast::send` is fire-and-forget, no blocking, no allocation pressure
- **Idempotent**: safe to apply on a clean checkout of commit `afc87b1`
- **Complete**: compiles and passes `cargo test --lib` (currently 591 tests)

## `#![forbid(unsafe_code)]`

All pipeline modules are under `#![forbid(unsafe_code)]`.

## Build & test

```bash
nix develop --extra-experimental-features 'nix-command flakes' --command cargo build
nix develop --extra-experimental-features 'nix-command flakes' --command cargo test --lib
```

## Deliverable

Generate downloadable code files for:
- `BroadcastReporter` implementation (in `src/convert/pipeline/reporter.rs` or a new file)
- Patches or replacement sections for `src/convert/processor.rs` (three call sites)
- Per-track `Progress` emissions in `src/convert/pipeline/stages.rs` (inside `convert_tracks` and other long-running stages)
- Stage→phase mapping + monotonic progress state
- Tests for the reporter and event emission points
