# Code task: Conservative ETA + cancellation visibility (v2 Milestones 6-7)

## Repo

https://github.com/barstoolbluz/tonepoet.git  
Branch: `main`, commit: `0cf807c`

## Context

Read these for background:

1. `docs/proposed-v2-roadmap-for-progress-system.txt` — full roadmap (Milestones 6-7)
2. `docs/progress_v2_milestone1_brief.md` — Milestone 1 (tracker foundation)
3. `docs/progress_v2_group_a_brief.md` — Milestones 2-5 (heartbeat + probes)

## What already exists

**Milestones 1-5 (commit `0cf807c`):**
- `OperationProgressTracker` with `measured`, `estimated`, `unknown_alive`, `start_unit`, `finish_unit`, `cancel_requested`, `failure` methods
- `ProgressThrottle` with 0.5% delta / 500ms interval coalescing
- `ProgressConfidence` enum (Measured/Estimated/Unknown)
- Elapsed-time formatting (`format_elapsed`: `48s`, `2m 14s`, `1h 03m`)
- Heartbeat wrapper for opaque operations
- Streaming child-process helper with ffmpeg/sox/archive probe parsers
- `BroadcastReporter` preserves last progress for failed/cancelled terminal states
- `cancel_requested()` already emits "Cancelling…" immediately with `force: true`
- Streaming helper already calls `cancel_requested()` on CancellationToken
- 645 tests green

## What this task delivers

### Milestone 6: Conservative ETA

An ETA engine that appends rough remaining-time estimates to progress messages when enough signal exists.

**Rules (from roadmap):**
- No ETA during the first unit unless tool-level (measured) progress exists
- Use duration/sample-weighted history when available, not raw item count
- Round aggressively: `about 7m remaining`, `about 1m remaining`
- Hide ETA after failures, skips, cancellation, or denominator changes
- Never display exact-looking times like `6m 43s remaining`

**Good examples:**
```
Converting track 2 of 5: Flamenco Sketches · about 7m remaining
Extracting archive · about 1m remaining
```

**Implementation approach:**
- Add an `EtaEstimator` struct (or integrate into `OperationProgressTracker`) that:
  - Accumulates elapsed time per completed unit
  - Weights by duration/sample count when available (from `PreparedTrack.expected_samples`)
  - Computes remaining = (avg_time_per_weighted_unit × remaining_units)
  - Rounds aggressively using a helper like `format_eta_coarse` → `about Xm remaining`
  - Resets/hides on failure, skip, cancellation, or total-count change
- The tracker appends ETA to progress messages when conditions are met
- ETA formatting lives in `src/convert/pipeline/progress/elapsed.rs` (extends existing module) or a new `eta.rs`

**Key design constraint:** ETA must be conservative. When in doubt, don't show it. Showing no ETA is better than a wrong one.

### Milestone 7: Cancellation visibility (polish)

Most of M7 is already implemented. The remaining gaps:

1. **Tool-specific cancel messages**: When cancelling during a streaming tool operation, show which tool is being stopped. Currently `cancel_requested()` emits generic "Cancelling…". The streaming helper knows the `ToolBinary` — it should emit "Stopping ffmpeg…" or "Stopping sox…" instead.

2. **"Cancelled at N%" final message**: `BroadcastReporter` already preserves last progress for cancelled terminals. Verify the TUI displays this correctly (it should from the v1 work). If the message field shows "Cancelled" without the percentage context, the progress message should include it: "Cancelled at 37%".

**Scope:** These are small changes — a `cancel_requested_with_tool` method or similar, plus a message format tweak. No architectural changes.

## File structure

New or modified files:
- `src/convert/pipeline/progress/eta.rs` (new) — `EtaEstimator`, `format_eta_coarse`
- `src/convert/pipeline/progress/operation.rs` — integrate ETA into the tracker
- `src/convert/pipeline/progress/mod.rs` — wire new module
- `src/convert/pipeline/progress/streaming.rs` — tool-specific cancel message (M7)

## Locked contracts (do not change)

- `PipelineEvent` enum
- `PipelineReporter` trait
- `ProgressUpdate` struct
- `ConversionStatus` enum

## Tests required

**M6 ETA:**
- ETA hidden during first unit without tool progress
- ETA appears after first unit completes with enough history
- ETA uses duration/sample weighting when available
- ETA falls back to item count when no duration data
- ETA rounded to coarse values (`about 7m remaining`, not `6m 43s`)
- ETA hidden after failure
- ETA hidden after cancellation
- ETA hidden after denominator change (skip)
- ETA not shown when progress is Unknown confidence

**M7 Cancellation:**
- Cancel during streaming tool emits tool-specific message
- Cancelled terminal preserves last-known progress percentage
- Cancel message includes the progress point ("Cancelled at 37%")

## `#![forbid(unsafe_code)]`

All pipeline modules are under `#![forbid(unsafe_code)]`.

## Quality requirements

The code must be:
- **Correct**: ETA is conservative — never shown without sufficient signal
- **Performant**: no heavy computation per progress event
- **Route-neutral**: ETA engine works for any stage, not just Convert
- **Idempotent**: safe to apply on a clean checkout of commit `0cf807c`
- **Complete**: compiles and passes `cargo test --lib` (currently 645 tests, must not regress)

## Build & test

```bash
nix develop --extra-experimental-features 'nix-command flakes' --command cargo build
nix develop --extra-experimental-features 'nix-command flakes' --command cargo test --lib
```

## Deliverable

Generate downloadable, production-ready code files:
- ETA estimator + coarse formatting (`eta.rs` or extension to `elapsed.rs`)
- Integration into `OperationProgressTracker`
- Tool-specific cancel messages in streaming helper
- Module wiring in `mod.rs`
- Tests for all ETA rules and cancellation polish
