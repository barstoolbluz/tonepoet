# Chunk 2: Orchestrator Unification

## For: Reasoning model (GPT Pro)
## Project: tonepoet — CLI + TUI audio conversion toolkit
## Language: Rust (edition 2021, async via Tokio)
## Quality bar: Rigor, correctness, robustness, idempotency, performance (in that order). Deterministic command construction (same inputs → same outputs). Idempotent where applicable: re-running the same conversion produces identical output; re-running after interruption does not leave corrupt state or orphan files.
## Prerequisite: Chunk 1 (tonepoet-pipeline crate) is complete and compiling. Its public API is in `tonepoet-pipeline/API_SURFACE.md`.

---

## 1. What this chunk does

Collapse three independent conversion paths into one unified orchestration flow. Every source type (single audio file, batch, SACD ISO, CUE+image, 7z archive) goes through the same orchestrator → chain planner → ToolRunner pipeline. The `tonepoet-pipeline` crate (Chunk 1) produces `PlannedCommand` lists. This chunk wires the orchestrator to consume them.

---

## 2. The three paths being unified

### Path A: Pipeline path (SACD, CUE, 7z)
- Entry: `processor.rs` → `run_sacd/cue/sevenzip_pipeline_conversion_item()`
- Orchestrator: `stages.rs` → `run_pipeline_item_with_tool_paths()`
- Stages: materialize → plan → convert → merge → metadata → ReplayGain → features → publish → log
- Encoding: `encode_command()` calls `CommandBuilder::new().build()` from backend crate — populates only 5 of 23 settings fields
- Execution: via `ToolRunner` trait (async, cancellation, timeout, streaming progress)

### Path B: Backend conversion path (single audio files)
- Entry: `processor.rs:1823` → `convert_with_backend()` from tonepoet-backend crate
- All settings mapped correctly
- Execution: direct `tokio::process::Command` — NOT through ToolRunner
- ConversionPipeline returned but never inspected

### Path C: Copy mode (FLAC→FLAC, no re-encode)
- Entry: `processor.rs:1789` → `copy_flac_with_full_pipeline()` (~350 lines)
- Does: fs::copy → rename → retag → lineage → ReplayGain → AAC fixup
- Duplicates post-processing logic from the other two paths

### After this chunk: ONE path
- All sources → `run_pipeline_item_with_tool_paths()` (or a renamed unified entry)
- `encode_command()` replaced by: call `tonepoet_pipeline::plan_conversion()`, convert each `PlannedCommand` to `ToolCommand`, execute via `ToolRunner`
- Copy mode becomes passthrough: `plan_conversion()` returns `PlanAction::PassthroughCopy`, orchestrator does `fs::copy`
- `convert_with_backend()` and `create_backend_conversion_item()` deleted
- `copy_flac_with_full_pipeline()` deleted
- Path B and C routing in `process_item()` replaced with pipeline dispatch

---

## 3. The bridge: PlannedCommand → ToolCommand

The pipeline crate produces `PlannedCommand`:
```rust
pub struct PlannedCommand {
    pub tool: ToolIdentifier,      // Ffmpeg, Sox, Ssrc, Loudgain, Metaflac, Flac, Custom(String)
    pub args: Vec<String>,
    pub input: InputSource,        // Path(PathBuf) or Stdin
    pub output: OutputSink,        // Path(PathBuf), Stdout, or InPlace(PathBuf)
    pub environment: BTreeMap<String, String>,
    pub expected_duration: Option<Duration>,
    pub description: String,
}
```

The orchestrator executes via `ToolCommand`:
```rust
pub struct ToolCommand {
    pub binary: ToolBinary,        // SevenZip, Ffmpeg, Ffprobe, Sox, Loudgain, Metaflac, ...
    pub args: Vec<String>,
    pub secret_args: Vec<usize>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<EnvVar>,
    pub timeout: Duration,
}
```

The adapter needs to:
1. Map `ToolIdentifier` → `ToolBinary` (add `Ssrc` and `Flac` variants to `ToolBinary`)
2. Map `PlannedCommand.args` → `ToolCommand.args`
3. Map `PlannedCommand.environment` → `ToolCommand.env` (as `EnvVar` with `SecretString`)
4. Set `timeout` from `expected_duration` or a default
5. Handle `Custom(String)` tool identifiers — either map to a new `ToolBinary::Custom(String)` variant or reject

---

## 4. ToolBinary changes needed

Current enum:
```rust
pub enum ToolBinary {
    SevenZip, Ffmpeg, Ffprobe, Sox, Loudgain, Metaflac,
    Opustags, Wvunpack, Wvtag, AtomicParsley,
}
```

Needs:
- `Ssrc` variant (for SSRC brick-wall resampling)
- `Flac` variant (for native FLAC decode verification via `flac -t -s`)
- Consider `Custom(String)` for future extensibility (matches pipeline crate's `ToolIdentifier::Custom`)

The `ToolBinary::canonical_name()` method maps variants to binary names for PATH lookup.

---

## 5. How the encode step changes

### Current flow (per track in convert_tracks):
```
realize_track() → encode_command() → run_streaming_tool_with_probe() → ToolOutput
```

### New flow (per track):
```
realize_track()
  → build SourceInfo from realized track
  → build PipelineSettings from unified request settings
  → plan_conversion() → ConversionPlan
  → match plan.action:
      PassthroughCopy → fs::copy
      Execute { commands, .. } → for each PlannedCommand:
          adapt to ToolCommand
          run via ToolRunner (with appropriate progress probe)
  → collect artifacts
```

### Progress reporting for multi-step chains
Current: one `run_streaming_tool_with_probe()` call per track with ffmpeg or sox progress parsing.

New: multiple ToolCommands per track (e.g., ffmpeg → ssrc → sox). Options:
- Run each step sequentially, divide the progress window proportionally by step count
- The `expected_duration` on each PlannedCommand helps weight the progress windows
- Probe selection still works: ffmpeg commands get ffmpeg probes, sox commands get sox probes, ssrc/other get heartbeat-only

### Error handling for multi-step chains
If step 2 of a 3-step chain fails:
- Report the failure with the specific step's stderr
- Clean up intermediate files listed in `ConversionPlan.cleanup_paths()`
- The track is marked failed; `FailurePolicy` determines if the album continues

---

## 6. How processor.rs simplifies

### Current routing in process_item():
```rust
if sacd_pipeline_candidate() → run_sacd_pipeline_conversion_item()
if cue_pipeline_policy() → run_cue_pipeline_conversion_item()
if 7z → run_sevenzip_pipeline_conversion_item()
if can_use_copy_mode() → copy_flac_with_full_pipeline()
else → convert_with_backend()
```

### New routing:
```rust
// Everything goes through the unified pipeline
let pipeline_req = build_pipeline_request(&item);
run_pipeline_item_with_tool_paths(pipeline_req, &runner, &reporter, &cancel, &tool_paths).await
```

The `build_pipeline_request()` function replaces the three separate builders (`pipeline_request_for_sacd_item`, `pipeline_request_for_cue_item`, `pipeline_request_for_7z_item`) by constructing a `PipelineRequest` that carries:
- Source type info (materializer selection)
- Full `PipelineSettings` from user's TUI selections (no field loss)
- Output path, naming policy, stage policy, etc.

Single audio files now go through the same pipeline — their "materializer" is trivially "the file itself" (no extraction needed). Copy mode is handled by `plan_conversion()` returning `PlanAction::PassthroughCopy`.

---

## 7. What gets deleted

| Code | Lines | Why |
|------|-------|-----|
| `encode_command()` | stages.rs:4622-4650 | Replaced by plan_conversion() |
| `dsd_to_pcm_command()` | stages.rs:4655-4684 | Replaced by SoxPlugin in pipeline crate |
| `backend_settings()` | stages.rs:4686-4700 | Replaced by PipelineSettings (no mapping needed) |
| `tool_command_from_backend()` | stages.rs:4717-4744 | Replaced by PlannedCommand → ToolCommand adapter |
| `convert_with_backend()` call | processor.rs:1823-1854 | Path B eliminated |
| `create_backend_conversion_item()` | processor.rs:1993-2187 | ~180 lines of type translation, eliminated |
| `copy_flac_with_full_pipeline()` | processor.rs:569-918 | ~350 lines, replaced by passthrough |
| `can_use_copy_mode()` | processor.rs:922-979 | Passthrough decided by plan_conversion() |
| Three separate pipeline request builders | processor.rs | Unified into one |
| `DitherPolicy` enum | types.rs | Replaced by DitherType from pipeline crate |

---

## 8. What stays (modified)

| Code | What changes |
|------|-------------|
| `run_pipeline_item_with_tool_paths()` | The stage sequence stays. Only the convert stage changes (calls pipeline crate instead of encode_command) |
| `convert_tracks_with_reporter_with_tool_paths()` | Track loop stays. Inner body changes: plan_conversion() + multi-step execution instead of single encode_command() |
| `ToolRunner` trait + `RealToolRunner` | Stays. Gains ability to run new ToolBinary variants (Ssrc, Flac) |
| `OperationProgressTracker` | Stays. Progress windowing adapts to multi-step chains |
| Materializers (SACD, CUE, 7z) | Stay as-is. They produce staged files; the pipeline crate encodes them |
| Metadata stage | Stays. Pipeline crate's MetadataDisposition may inform whether to skip it |
| ReplayGain stage | Stays. Pipeline crate plans ReplayGain commands; orchestrator may delegate to those or keep its own loudgain invocation |
| Post-processing (features, publish, log) | Stay unchanged |

---

## 9. How PipelineRequest evolves

Current PipelineRequest (types.rs) carries `EncodeOptions` with 4 fields. It needs to carry the full `PipelineSettings` from the pipeline crate instead.

Option A: Replace `EncodeOptions` with `tonepoet_pipeline::PipelineSettings` directly.
Option B: Keep PipelineRequest as the orchestrator's internal type, with a method that builds a `tonepoet_pipeline::PlanRequest` for each track.

Option B is likely better — PipelineRequest carries orchestration concerns (job_id, output_root, naming policy, stage policy, failure policy) that the pipeline crate doesn't know about. The pipeline crate only needs per-track `PlanRequest` (input path, output path, source info, settings).

---

## 10. Concurrency model

Design and implement a shared worker pool with work-stealing semantics. This replaces the current serial track encoding and simplistic inter-album parallelism.

### 10.1 Worker pool

- **Pool size**: `cores - 1` by default (e.g., 15 workers on a 16-thread system). Configurable via `config.toml` (`worker_count`).
- **No hard partitioning.** Workers are not reserved for specific jobs. As a worker finishes a unit of work, it picks up the next available unit from the queue.
- **Non-dependent work starts immediately** as workers free up. If the queue has 5 individual FLACs + 2 SACDs + 1 7z, individual tracks start encoding while archives extract.

### 10.2 Two-phase jobs

Multi-track sources (archives, ISOs, CUE+image) have two phases:

**Phase 1 — Materialization** (extraction/splitting):
- **Archives (7z, ZIP, RAR, future TAR)**: a single invocation of the extraction tool. We ship the fastest available tools (7zz for 7-Zip, not p7zip; fast unrar when RAR support is added). The tool handles its own internal threading. Occupies 1 worker slot for the invocation, but the tool may use additional threads internally — we don't manage that.
- **SACD ISOs**: sacd-rs's `extract_track()` takes `&mut IsoReader` (wraps a single `File` handle). Multiple tracks from the same ISO can extract concurrently by opening separate `IsoReader` instances for the same path — tracks read non-overlapping LSN ranges. Performance depends on storage (SSD = good, HDD = I/O contention). Parallelize extraction across available workers.
- **CUE+image splitting**: each track is an independent ffmpeg call. Fully parallelizable — each split is a work unit in the pool.

**Phase 2 — Encoding**:
- Always parallel. Each extracted/split track is an independent work unit in the pool.
- A track's multi-step chain (e.g., ffmpeg → ssrc → sox) is sequential within that track, but multiple tracks' chains run concurrently.

### 10.3 Job scheduling

- Each queue item is a **job**: single file, archive, ISO, or CUE+image.
- Single audio files: 1 work unit each, immediately eligible for the pool.
- Multi-track jobs: materialization produces N tracks, each becomes a work unit for encoding.
- The scheduler does not wait for one job to fully complete before starting another. As tracks become available from materialization, they enter the encoding pool alongside tracks from other jobs.
- **Completion order**: tracks complete in whatever order workers finish them. No ordering requirement unless dependencies exist (e.g., ReplayGain album mode requires all tracks to finish before analysis).

### 10.4 Post-processing synchronization

Some post-processing steps require all tracks in an album to be encoded first:
- **ReplayGain (album mode)**: needs all tracks to compute album-level gain.
- **Merge**: concatenates all tracks into a single file.
- **Album-level logging/publishing**: runs after all tracks complete.

The scheduler must track per-album completion and trigger these steps only when all tracks in the album are done.

### 10.5 Example scenarios

**100 individual tracks, 15 workers**: all 100 enter the pool. 15 encode concurrently. As each finishes, the next starts. No materialization phase.

**2 SACD ISOs, 15 workers**: both ISOs start extracting concurrently (2 workers, or more if we parallelize per-track extraction). As tracks are extracted, they enter the encoding pool. With 13 remaining workers, ~13 tracks encode concurrently across both albums.

**1 7z archive with 20 tracks, 15 workers**: 7zz extracts all 20 tracks (1 invocation, 7zz uses internal threading). After extraction completes, 15 tracks encode concurrently, then the remaining 5.

**5 FLACs + 2 SACDs + 1 7z, 15 workers**: FLACs start encoding immediately (5 workers). SACDs start extracting (2 workers). 7z starts extracting (1 worker). 7 workers busy. As FLAC encodes finish, workers pick up SACD/7z tracks as they become available from extraction.

---

## 11. Design constraints

1. **The pipeline crate is done.** Use its public API as-is. Do not modify it.
2. **ToolRunner is the execution boundary.** Every process spawned goes through ToolRunner. No direct `std::process::Command` calls.
3. **Progress reporting must work for multi-step chains.** The user sees smooth progress, not N separate 0-100% bars.
4. **Cleanup on failure/interruption.** `ConversionPlan.cleanup_paths()` lists intermediate files to remove.
5. **The materializers are untouched.** They produce staged files. The new encode step consumes them.
6. **Copy mode is not special.** The pipeline crate decides passthrough. The orchestrator just does `fs::copy` + post-processing when told.
7. **Single audio files use the same path.** No separate `convert_with_backend()`. A single FLAC file gets a trivial "materializer" (itself) and goes through plan_conversion().
8. **Fast extraction tools.** We ship 7zz (official 7-Zip, not p7zip) and will ship fast unrar. The scheduler does not manage internal threading of external tools — it accounts for them as occupying a worker slot.
9. **Concurrency is built in, not deferred.** The worker pool and scheduling model described in Section 10 are part of this chunk's deliverables.

---

## 12. Deliverables

1. **The PlannedCommand → ToolCommand adapter** — function signature, ToolIdentifier → ToolBinary mapping, env var handling.

2. **ToolBinary enum changes** — new variants (Ssrc, Flac, possibly Custom), canonical_name() additions.

3. **The new encode step** in convert_tracks — how it calls plan_conversion(), iterates PlannedCommands, handles multi-step progress, handles PassthroughCopy.

4. **The unified PipelineRequest** — how it carries PipelineSettings, how it constructs per-track PlanRequest for the pipeline crate.

5. **The unified build_pipeline_request()** function — replaces three separate builders + the backend conversion item mapper.

6. **How processor.rs dispatch simplifies** — the new routing logic that sends everything through the pipeline.

7. **Progress reporting for multi-step chains** — how OperationProgressTracker adapts to N commands per track.

8. **Error handling** — what happens when a multi-step chain fails mid-way, cleanup strategy.

9. **How metadata/ReplayGain stages interact** — does the orchestrator use the pipeline crate's planned ReplayGain/metadata commands, or keep its own? How does MetadataDisposition inform this?

10. **Worker pool and scheduler design** — the shared pool struct, how jobs are submitted, how work units are dispatched to workers, how per-album completion is tracked for post-processing synchronization (ReplayGain album mode, merge, publish).

11. **How materialization feeds the pool** — archive extraction (single invocation, tool handles threading) vs SACD (parallel IsoReaders) vs CUE (parallel ffmpeg splits) vs single files (no materialization). How extracted tracks enter the encoding pool.

12. **Sequenced implementation plan** — which files change, in what order, with each step leaving the system compiling and tests passing.

For each item: struct definitions, function signatures, data flow. Include error types where relevant. Do not rewrite the pipeline crate — use its API.

---

## 13. Code files included in zip

| File | Description |
|------|-------------|
| `docs/chunk2_orchestrator_brief.md` | This brief |
| `tonepoet-pipeline/API_SURFACE.md` | Pipeline crate public API (Chunk 1 output) |
| `tonepoet-pipeline/ARCHITECTURE.md` | Pipeline crate architecture |
| `tonepoet-pipeline/src/plan.rs` | Chain planner: PlanRequest, ConversionPlan, PlannedCommand |
| `tonepoet-pipeline/src/tools.rs` | ToolPlugin trait, ToolRegistry, ToolIdentifier |
| `tonepoet-pipeline/src/settings.rs` | PipelineSettings (unified settings type) |
| `tonepoet-pipeline/src/source.rs` | SourceInfo (what the planner needs about input) |
| `tonepoet-pipeline/src/enums.rs` | All unified enums (AudioFormat, DitherType, etc.) |
| `src/convert/pipeline/tool.rs` | ToolBinary, ToolCommand, ToolRunner trait (full file) |
| `src/convert/pipeline/types.rs` | Current PipelineRequest, EncodeOptions (full file) |
| `chunk2_excerpts/stages_encode.rs` | encode_command + dsd_to_pcm + backend_settings (lines 4622-4744) |
| `chunk2_excerpts/stages_convert_tracks.rs` | Track loop (lines 1280-1667) |
| `chunk2_excerpts/stages_orchestrator.rs` | run_pipeline_item + stage sequence (lines 3381-3860) |
| `chunk2_excerpts/processor_dispatch.rs` | process_item routing + three pipeline wrappers (excerpts) |
| `chunk2_excerpts/processor_legacy.rs` | convert_with_backend call site + copy mode + backend item mapper (excerpts) |
