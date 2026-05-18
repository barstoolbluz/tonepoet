# Code task: Heartbeat + tool progress probes (v2 Milestones 2-5)

## Repo

https://github.com/barstoolbluz/tonepoet.git  
Branch: `main`, commit: `70f7b67`

## Context

Read these documents for background:

1. `docs/proposed-v2-roadmap-for-progress-system.txt` — full v2 roadmap (Milestones 2-5 are in scope here)
2. `docs/progress_v2_milestone1_brief.md` — Milestone 1 brief (already implemented)

## What already exists

**Milestone 1 (commit `70f7b67`):**
- `src/convert/pipeline/progress/` module with:
  - `OperationProgressTracker` — async methods: `start_unit`, `measured`, `estimated`, `unknown_alive`, `finish_unit`, `cancel_requested`, `failure`
  - `ProgressThrottle` — source-side coalescing (0.5% delta or 500ms interval, material key separation)
  - `ProgressConfidence` enum (Measured/Estimated/Unknown)
  - Elapsed-time formatting with 5-second threshold
- `BroadcastReporter` in `reporter.rs` — maps `PipelineEvent` → `ProgressUpdate` for TUI
- `convert_tracks` in `stages.rs` already uses the tracker for per-track progress
- 626 tests green

## What this task delivers

Four milestones that make long-running work visible:

### Milestone 2: Heartbeat for opaque operations

A timer wrapper that calls `tracker.unknown_alive("still running")` periodically during operations that cannot report progress.

**Where needed:**
- SACD DSD extraction (`stages.rs` ~line 627): `tokio::task::spawn_blocking` runs in-process `sacd_rs::extract_track`. No stderr to parse. This is the 162-second frozen case.
- Any future in-process operation with no progress signal.

**Implementation:** Spawn a `tokio::task` with `tokio::time::interval(Duration::from_secs(10))` that calls `tracker.unknown_alive()`. Cancel it (via a shared flag or dropping a channel) when the operation completes.

**Does not need streaming stderr. Does not need ToolRunner changes. Simplest of the four milestones.**

### Milestone 3: ffmpeg progress probe

Parse ffmpeg's stderr during encoding to get measured progress.

**Progress source:** ffmpeg writes `time=HH:MM:SS.ms` to stderr during conversion. Compare against expected duration → `tracker.measured(progress, message)`.

**Where needed:** `encode_command` in `stages.rs` builds ffmpeg commands for PCM encoding. The `runner.run(cmd, cancel)` call at `stages.rs:1358` is the target site.

### Milestone 4: sox progress probe

Parse sox's stderr progress output, or fall back to honest unknown.

**Progress source:** sox with `-S` flag writes progress to stderr. For DSD→PCM conversion, the `dsd_to_pcm_command` function (`stages.rs` ~line 2846) builds the sox command. Add `-S` to the args.

**Fallback:** When sox progress output is not parseable or unavailable: `tracker.unknown_alive("progress unavailable from sox · still running")`.

### Milestone 5: archive extraction progress

Parse 7z extraction output for file-level progress.

**Progress source:** 7z writes `Extracting  filename` lines to stdout during extraction. Parse file count or current member name.

**Where needed:** `extract_archive` in `materializer_7z.rs` runs the 7z command through ToolRunner.

**Fallback:** If output is not parseable: `tracker.unknown_alive("Extracting archive · still running")`.

## Architectural challenge: streaming stderr

The current `RealToolRunner::run` (in `tool.rs`) pipes stdout/stderr and reads them to completion via `read_tail` after the process exits. It does NOT provide streaming access during execution.

Milestones 3-5 need to read stderr/stdout lines while the child process is running.

**Recommended approach:** Create a shared streaming child-process helper in the progress module that:
1. Spawns the child process (same args/env/cwd/timeout/cancel as ToolRunner)
2. Reads stderr (or stdout) line-by-line in a streaming loop
3. Feeds each line to a probe parser callback
4. Collects the tail for the final `ToolOutput` / `CommandRecord`
5. Handles timeout and cancellation (same semantics as `RealToolRunner`)
6. Returns `Result<ToolOutput, ToolRunnerError>` for compatibility with existing call sites

This bypasses `ToolRunner::run` for progress-enabled commands but produces the same output types. The existing `ToolRunner` trait is unchanged.

**Suggested file:** `src/convert/pipeline/progress/streaming.rs`

**Milestone 2 (heartbeat) does NOT need this** — it's purely timer-based.

## Probe parser design

Each probe is a stateless line parser:

```
src/convert/pipeline/progress/probes/
  mod.rs
  ffmpeg.rs   — parse `time=HH:MM:SS.ms`, compute progress from duration
  sox.rs      — parse sox `-S` output, or return None for unparseable lines
  archive.rs  — parse `Extracting  filename` lines, track file count
```

Each parser is a function: `fn parse_line(line: &str) -> Option<ProbeResult>` where `ProbeResult` carries the parsed progress or file identity. The streaming helper calls the parser for each line and feeds results to the `OperationProgressTracker`.

## What does NOT change

- `ToolRunner` trait (locked by practice, bypassed not modified)
- `PipelineEvent`, `PipelineReporter`, `ProgressUpdate`, `ConversionStatus` (locked contracts)
- `OperationProgressTracker` API (Milestone 1, already shipped)
- `BroadcastReporter` (unchanged)

## Integration points

| Milestone | Code site | Currently | After |
|---|---|---|---|
| M2 heartbeat | `stages.rs` ~line 627 (`spawn_blocking` for SACD) | No progress for 162s | "still running" every 10s |
| M3 ffmpeg | `stages.rs` ~line 1358 (encode via ToolRunner) | No mid-encode progress | Measured % from `time=` |
| M4 sox | `stages.rs` ~line 2846 (`dsd_to_pcm_command`) | No mid-encode progress | Measured % from `-S` or honest unknown |
| M5 archive | `materializer_7z.rs` ~line 153 (7z via ToolRunner) | No mid-extract progress | File count or member name |

## Tests required (from roadmap)

- Heartbeat emits after configured interval
- Heartbeat does not advance progress without data
- ffmpeg parser handles normal progress lines (`time=00:02:14.56`)
- ffmpeg parser handles malformed lines safely (no panic, returns None)
- sox unknown-progress fallback emits useful messages
- sox parser handles `-S` progress output when available
- archive parser reports current file/member name
- archive parser handles 7z output variations safely
- Streaming helper handles timeout correctly
- Streaming helper handles cancellation correctly
- All probes feed through `OperationProgressTracker` (integration test)

## `#![forbid(unsafe_code)]`

All pipeline modules are under `#![forbid(unsafe_code)]`.

## Quality requirements

The code must be:
- **Correct**: parsers handle malformed input without panic, heartbeat doesn't advance progress
- **Performant**: line parsing is O(1) per line, no regex compilation per line
- **Route-neutral**: probe parsers are isolated; streaming helper is shared
- **Idempotent**: safe to apply on a clean checkout of commit `70f7b67`
- **Complete**: compiles and passes `cargo test --lib` (currently 626 tests, must not regress)

## Build & test

```bash
nix develop --extra-experimental-features 'nix-command flakes' --command cargo build
nix develop --extra-experimental-features 'nix-command flakes' --command cargo test --lib
```

## Deliverable

Generate downloadable, production-ready code files:
- Heartbeat wrapper (can live in `src/convert/pipeline/progress/heartbeat.rs`)
- Streaming child-process helper (`src/convert/pipeline/progress/streaming.rs`)
- Probe parsers (`src/convert/pipeline/progress/probes/ffmpeg.rs`, `sox.rs`, `archive.rs`)
- Patches for integration sites (`stages.rs` for M2/M3/M4, `materializer_7z.rs` for M5)
- Tests for all probes, heartbeat, and streaming helper
