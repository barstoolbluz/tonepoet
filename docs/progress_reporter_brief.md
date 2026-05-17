# Brief: Pipeline → TUI progress reporting

## Repo

https://github.com/barstoolbluz/tonepoet.git  
Branch: `main`, commit `afc87b1`.

## Problem

The conversion pipeline emits `PipelineEvent` progress events via a `PipelineReporter` trait. The TUI displays progress via `ProgressUpdate` structs sent through a `broadcast::Sender<ProgressUpdate>`. Currently all three pipeline routing paths (7z, CUE, SACD) in `src/convert/processor.rs` use `RecordingReporter` which captures events in memory but never forwards them to the TUI. Users see a frozen "0% Extracting" for the entire duration of a conversion.

## What needs to happen

Replace `RecordingReporter` with a `BroadcastReporter` that maps `PipelineEvent` → `ProgressUpdate` and sends them through the existing broadcast channel. The three call sites in `processor.rs` that create the reporter need to pass the broadcast sender so events flow to the TUI in real time.

## Architecture

**Source events** (`src/convert/pipeline/reporter.rs`):
```rust
pub enum PipelineEvent {
    StageStarted { item_id: String, stage: PipelineStage },
    StageFinished { item_id: String, record: StageRecord },
    Progress { item_id: String, stage: PipelineStage, phase_progress: f32, message: Option<String> },
    Terminal { item_id: String, status: ConversionStatus },
}
```

**Target struct** (`src/convert/processor.rs`):
```rust
pub struct ProgressUpdate {
    pub item_id: String,
    pub progress: f32,       // 0-100 overall
    pub status: ConversionStatus,
}
```

**TUI consumer** (`src/tui/event_loop.rs:173`): receives `AppMessage::ConversionProgress { item_id, status }` and updates the queue item.

## Stage → Phase mapping

The TUI uses `ConversionPhase` for display. Map pipeline stages to these phases and their progress windows:

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

Within the Convert stage, `PipelineEvent::Progress` carries `phase_progress` (0.0–1.0 per track). Scale this into the 20–80% window based on track index / total tracks.

## Three call sites to change

All in `src/convert/processor.rs`:

1. **7z path** (~line 1364): `let reporter = RecordingReporter::new();`
2. **SACD path** (~line 1103): `let reporter = RecordingReporter::new();`
3. **CUE path** (~line 1142): `let reporter = RecordingReporter::new();`

Each already has access to `progress_tx: broadcast::Sender<ProgressUpdate>` from the caller.

## What to build

A `BroadcastReporter` struct that:
1. Implements `PipelineReporter` (the `async fn emit(&self, event: PipelineEvent)` trait)
2. Holds a `broadcast::Sender<ProgressUpdate>` and the `item_id`
3. On each `PipelineEvent`, maps stage → phase, computes overall progress percentage, and sends a `ProgressUpdate`
4. Ignores send errors (receiver may be dropped — `let _ = tx.send(...)`)
5. Does NOT block — `broadcast::Sender::send` is non-blocking

## Constraints

- `PipelineReporter` trait requires `Send + Sync` — `broadcast::Sender` is both
- The reporter must not slow down the pipeline — sends should be fire-and-forget
- `RecordingReporter` should still exist for unit tests (don't delete it)
- No changes to `PipelineEvent`, `PipelineReporter` trait, `ProgressUpdate`, or `ConversionStatus` (all are locked contracts)
- `#![forbid(unsafe_code)]` applies

## Real-world urgency

A Miles Davis SACD (2 tracks, DSD64) took 162 seconds per track for sox DSD→PCM decimation. The user saw "0% Extracting" frozen for over 5 minutes with no indication of progress or even that the conversion was running. This is the primary UX blocker.

## Important: current event granularity

The orchestrator currently emits `StageStarted` and `StageFinished` events but does NOT emit `PipelineEvent::Progress` mid-stage. The `Progress` variant exists in the enum but is never used. This means progress jumps between stage boundaries (0% → 15% → 20% → 80%) rather than updating smoothly within the Convert stage.

For acceptable UX, the reasoning model should consider adding `Progress` emissions inside `convert_tracks` in `stages.rs` — one per track completion — so the Convert stage (20–80%) shows incremental per-track progress.

## Performance

- `broadcast::send` is O(1), non-blocking, no allocation
- The TUI event loop polls the receiver at ~60fps (crossterm tick)
- Pipeline events are sparse (one per stage start/finish, plus per-track if added)
- No concern about flooding

## Deliverable

Generate downloadable, production-ready code files. The code must be correct, performant, and idempotent (safe to apply on a clean checkout of the target commit). Files:

- `BroadcastReporter` implementation (can go in `src/convert/pipeline/reporter.rs` or a new file)
- Patches for the three call sites in `src/convert/processor.rs`
- The stage→phase mapping logic
- Per-track `Progress` emissions inside `convert_tracks` in `src/convert/pipeline/stages.rs` (if adding mid-stage granularity)

The code must compile and pass `cargo test --lib` when applied to the current `main` branch. No regressions to existing tests.

## Build & test

```bash
nix develop --extra-experimental-features 'nix-command flakes' --command cargo build
nix develop --extra-experimental-features 'nix-command flakes' --command cargo test --lib
```

Current state: 591 tests green.
