# Pipeline Redesign Brief

## For: Reasoning model (GPT Pro)
## Project: tonepoet — CLI + TUI audio conversion toolkit
## Language: Rust (edition 2021, async via Tokio)
## Quality bar: Rigor, correctness, robustness, idempotency, performance (in that order). Deterministic command construction (same inputs → same outputs). Idempotent where applicable: re-running the same conversion produces identical output; re-running after interruption does not leave corrupt state or orphan files.

---

## 1. What tonepoet does

tonepoet converts audio files between formats: FLAC, WAV, AIFF, WavPack, MP3, AAC, Opus, ALAC, DSF, DFF, and more. It extracts multi-track sources (SACD ISOs, CUE+image files, 7z archives), applies sample rate conversion, bit depth conversion, dithering, noise shaping, ReplayGain analysis, metadata preservation, and file renaming via tag-based templates.

The user configures conversion settings in a TUI (format, sample rate, bit depth, dither algorithm, ReplayGain mode) and queues files for conversion. The pipeline processes each file or album through a sequence of stages.

---

## 2. What exists today

### 2.1 Workspace structure

```
tonepoet/                          # Main crate (CLI + TUI + orchestrator)
├── src/
│   ├── main.rs                    # CLI entry point
│   ├── tui/                       # ratatui TUI (convert screen, format pills, etc.)
│   ├── convert/
│   │   ├── processor.rs           # Queue processing + conversion dispatch (~2,200 lines)
│   │   ├── pipeline/
│   │   │   ├── stages.rs          # Pipeline orchestrator (~6,200 lines)
│   │   │   ├── types.rs           # PipelineRequest, EncodeOptions, etc.
│   │   │   ├── tool.rs            # ToolBinary enum, ToolCommand, ToolRunner trait
│   │   │   ├── reporter.rs        # PipelineReporter, progress events
│   │   │   ├── progress/          # OperationProgressTracker, streaming probes, ETA
│   │   │   ├── materializer_sacd.rs
│   │   │   ├── materializer_cue.rs
│   │   │   └── materializer_7z.rs
│   │   ├── formats.rs             # AudioFormat, ConversionOptions, QualitySettings
│   │   ├── queue.rs               # ConversionQueue, ConversionItem
│   │   └── simple_wizard.rs       # DitherType, NyquistTransition, ReplayGainMode enums
├── crates/
│   ├── tonepoet-backend/          # Legacy backend crate (~6,800 lines)
│   │   ├── src/
│   │   │   ├── ffmpeg.rs          # FFmpegBuilder — constructs ffmpeg command lines
│   │   │   ├── sox.rs             # SoxBuilder — constructs sox command lines
│   │   │   ├── mapping.rs         # Parameter mapping (dither→args, quality→flags, etc.)
│   │   │   ├── types.rs           # ConversionSettings, AudioFormat, DitherType (DUPLICATES)
│   │   │   ├── pipeline.rs        # Multi-stage pipeline orchestrator (~2,300 lines, DUPLICATE)
│   │   │   ├── integration.rs     # Main* type hierarchy for crate boundary (~400 lines, BOILERPLATE)
│   │   │   ├── integration_api.rs # convert_with_backend() entry point (~310 lines)
│   │   │   ├── metadata.rs        # Format-specific metadata extractors/appliers (~1,600 lines)
│   │   │   ├── preset.rs          # Preset TOML loading
│   │   │   └── validation.rs      # Settings validation
│   ├── tonepoet-features/         # Log writer + CUE sheet generator
│   ├── tonepoet-wizard/           # Legacy TUI wizard (standalone, no backend deps)
│   └── sacd-rs/                   # SACD ISO parser + DSD extractor
```

### 2.2 Three conversion paths (the problem)

Today there are THREE independent code paths for converting audio, each with different capabilities:

#### Path A: Pipeline path (SACD, CUE, 7z sources)

- **Entry:** `processor.rs` → `run_sacd_pipeline_conversion_item()` / `run_cue_pipeline_conversion_item()` / `run_sevenzip_pipeline_conversion_item()`
- **Orchestrator:** `stages.rs` → `run_pipeline_item_with_tool_paths()`
- **Stages:** materialize → plan → convert → metadata → ReplayGain → features → publish → log
- **Encoding:** `encode_command()` in stages.rs calls `CommandBuilder::new(backend).build(input, output, &settings)` from the backend crate
- **Settings gap:** `backend_settings()` only populates 5 of ~25 fields in `BackendConversionSettings`:
  - format ✓
  - compression_level ✓
  - bitrate ✓
  - mp3_mode ✓
  - dither (as DitherPolicy::Auto — always, ignoring user's choice) ✓
  - sample_rate ✗ NOT SET
  - bit_depth ✗ NOT SET
  - resample_quality ✗ NOT SET
  - nyquist_transition ✗ NOT SET
  - dither_type (specific algorithm) ✗ NOT SET
  - ssrc_insane_mode ✗ NOT SET
- **DSD handling:** Hardcoded `dsd_to_pcm_command()` — always `sox -b 24 rate -v <target>`, ignoring user settings
- **SSRC:** Not available (ToolBinary has no SSRC variant)
- **Metadata:** Via format-specific tools (metaflac, opustags, wvtag, ffmpeg -metadata) through ToolRunner
- **Progress:** OperationProgressTracker with streaming probes for ffmpeg/sox stderr
- **Execution:** ToolRunner trait (async, cancellation, timeout, streaming progress)

#### Path B: Backend conversion path (single audio files)

- **Entry:** `processor.rs:1823` → `convert_with_backend()` from tonepoet-backend crate
- **Settings:** ALL ConversionOptions fields properly mapped:
  - target_sample_rate ✓
  - target_bit_depth ✓
  - dither_type (full 11-variant enum) ✓
  - resample_quality ✓
  - nyquist_transition ✓
  - ssrc_insane_mode ✓
  - preferred_backend ✓
- **Orchestrator:** Backend crate's `PipelineBuilder::build_pipeline()` → `ConversionPipeline`
- **SSRC:** Full brick-wall support via `build_ssrc_command()` (~100 lines)
- **Multi-stage chains:** Supports decode → SSRC → dither → encode as separate commands with temp files
- **Metadata:** MetadataPreservingPipeline (extract → convert → reapply)
- **Progress:** Own async execution with ProgressCallback
- **Execution:** Direct `std::process::Command` / `tokio::process::Command` — NOT through ToolRunner

#### Path C: Copy mode path (FLAC→FLAC, no re-encode)

- **Entry:** `processor.rs:1789` → `copy_flac_with_full_pipeline()` (~350 lines)
- **Gate:** `can_use_copy_mode()` checks: same format, no resample, no bit depth change, reencode_flac=false
- **Does:** fs::copy → rename → retag (metaflac) → lineage embed → ReplayGain (loudgain) → AAC fixup
- **No encoding** — just file copy + post-processing
- **Duplicates** rename, retag, ReplayGain, and lineage logic from the other two paths

### 2.3 Type duplication

Three parallel type hierarchies represent the same concepts:

| Concept | Main crate (simple_wizard.rs) | Main crate (pipeline/types.rs) | Backend crate (types.rs) | Backend crate (integration.rs) |
|---------|------------------------------|-------------------------------|--------------------------|-------------------------------|
| Dither algorithm | `DitherType` (11 variants) | `DitherPolicy` (3 variants: Auto/Off/On) | `DitherType` (9 variants) | `MainDitherType` (11 variants) |
| Audio format | `AudioFormat` (10 variants) | — | `AudioFormat` (8 variants) | `MainAudioFormat` (8 variants) |
| Nyquist transition | `NyquistTransition` (3 variants) | — | `NyquistTransition` (5 variants) | `MainNyquistTransition` (3 variants) |
| AAC profile | `AacProfile` (3 variants) | — | `AacProfile` (4 variants) | `MainAacProfile` (3 variants) |
| ReplayGain mode | `ReplayGainMode` (3 variants) | — | `ReplayGainMode` (3 variants) | `MainReplayGainMode` (3 variants) |
| Settings | `ConversionOptions` (23 fields) | `EncodeOptions` (4 fields) | `ConversionSettings` (23 fields) | `MainConversionOptions` (14 fields) |

`DitherPolicy` in the pipeline types is particularly problematic — it collapses 11 specific algorithms into 3 values (Auto/Off/On), discarding the user's choice.

### 2.4 What works well (KEEP)

- **Backend crate command builders** — `FFmpegBuilder::build()` and `SoxBuilder::build()` correctly construct command lines from `ConversionSettings`. ~460 lines, well-tested.
- **Backend crate mapping functions** — `mapping.rs` correctly maps dither types, resample quality, Nyquist transition, SSRC profiles to tool-specific arguments. ~264 lines.
- **Main crate orchestrator structure** — The materialize → plan → convert → metadata → RG → features → publish → log sequence in stages.rs is sound.
- **Main crate ToolRunner** — Async process execution with cancellation, timeout, streaming progress, bounded output capture. Robust.
- **Main crate materializers** — SACD (via sacd-rs), CUE, 7z extraction. Working, tested.
- **Progress system** — OperationProgressTracker, streaming probes for ffmpeg/sox, heartbeat, ETA estimation.
- **Naming/template system** — Tag-based folder/filename templates with custom variables.
- **Label resolver** — Dictionary-based label/pressing/artist canonicalization.

### 2.5 What's broken (REPLACE)

- **backend_settings()** in stages.rs — populates 5 of 23 fields, losing user settings
- **DitherPolicy enum** — lossy 3-value wrapper that discards the user's specific dither algorithm
- **PipelineRequest.EncodeOptions** — lacks sample_rate, bit_depth, dither_type, resample_quality, nyquist_transition, ssrc_insane_mode, preferred_backend
- **Hardcoded dsd_to_pcm_command()** — ignores user's target rate/depth/dither settings
- **DitherPolicy::Auto hardcoded** in 5 locations across processor.rs and command.rs
- **Backend crate pipeline.rs** (~2,300 lines) — duplicates orchestration that stages.rs does differently, with its own execution model
- **Backend crate integration.rs + integration_api.rs** (~710 lines) — type translation boilerplate for the Main* hierarchy
- **convert_with_backend()** — the whole function exists to bridge two type systems that shouldn't both exist
- **create_backend_conversion_item()** (~180 lines in processor.rs) — boilerplate mapping main types to backend Main* types
- **copy_flac_with_full_pipeline()** (~350 lines) — duplicates rename, retag, ReplayGain, lineage logic
- **Three separate progress reporting models** — OperationProgressTracker, ProgressCallback, send_phase_update()
- **store_md5 bug** — ffmpeg.rs:276-281 writes ID3v2 tags to FLAC files (should be vorbis comments)
- **Dead fields** — `original_settings` in ConversionOptions (never populated, never read)

### 2.6 ToolBinary enum (current)

```rust
pub enum ToolBinary {
    SevenZip, Ffmpeg, Ffprobe, Sox, Loudgain, Metaflac,
    Opustags, Wvunpack, Wvtag, AtomicParsley,
}
```

No SSRC variant. The ToolRunner can only execute tools in this enum. Adding SSRC to the pipeline path requires adding a variant here.

### 2.7 Verified facts

All of the following were verified by reading the actual code, with file paths and line numbers confirmed:

- PipelineRequest has NO fields for target_sample_rate or target_bit_depth (types.rs:56-78)
- DitherPolicy::Auto is hardcoded in 5 locations (processor.rs:1055,1183,1378,1658; command.rs:3910)
- backend_settings() extracts 5 fields from PipelineRequest (stages.rs:4686-4700)
- The legacy single-file path maps ALL ConversionOptions fields (processor.rs:2120-2177)
- Backend crate's PipelineBuilder is NEVER called from the main crate (confirmed via grep)
- ConversionPipeline returned by convert_with_backend() is NEVER inspected (processor.rs:1834)
- The TUI has zero imports from tonepoet-backend (confirmed via grep across src/tui/)
- reencode_flac is functional — gates FLAC copy mode (processor.rs:936)
- SSRC is a build dependency (flake.nix) with full command construction in backend pipeline.rs:594-694
- preferred_backend is settable via CLI --backend flag (main.rs:497) but not from the TUI
- Lineage embedding works in both copy mode and backend path but with confusing dual naming
- MetadataPreservingPipeline in backend crate is dead code from the main crate's perspective

---

## 3. Target architecture

### 3.1 New crate: tonepoet-pipeline

A new crate that owns:

1. **The single settings type** — one `ConversionSettings` struct, no duplicates across crates
2. **The AudioFormat enum** — one definition, used everywhere
3. **The DitherType enum** — one definition with all variants, no lossy wrappers
4. **Command builders** — FFmpegBuilder, SoxBuilder (migrated from backend crate, they work)
5. **Chain planner** — given settings + source info, produces an ordered list of commands (single ffmpeg call, or decode → SSRC → dither → encode, etc.)
6. **Mapping functions** — migrated from backend crate's mapping.rs
7. **The encoder trait** — extensible interface for adding new output formats/tools without modifying the crate

The pipeline crate is **pure** — it takes settings in, produces commands out. It does not execute anything, does not know about ToolRunner, does not know about the TUI, does not spawn processes.

### 3.2 Main crate changes

- **One orchestration path** — stages.rs handles ALL sources (SACD, CUE, 7z, single files, batch)
- **Copy mode** becomes a passthrough encoder, not a separate 350-line function
- **encode_command() replaced** — calls the pipeline crate's chain planner, converts the resulting commands to ToolCommands, executes via ToolRunner
- **ToolBinary gains SSRC variant** — enabling SSRC execution through ToolRunner with cancellation/timeout/progress
- **PipelineRequest carries full settings** — no information loss between TUI and backend
- **DitherPolicy deleted** — DitherType used everywhere

### 3.3 Backend crate disposition

After migration to tonepoet-pipeline:
- **ffmpeg.rs, sox.rs, mapping.rs** → move to tonepoet-pipeline (these are the leaf nodes)
- **types.rs core enums/structs** → move to tonepoet-pipeline
- **metadata.rs** → stays or moves to its own crate (format-specific metadata tools are still useful)
- **pipeline.rs** → deleted (orchestration lives in main crate's stages.rs)
- **integration.rs, integration_api.rs** → deleted (no more type translation layer)
- **preset.rs, validation.rs** → evaluate: move useful parts to tonepoet-pipeline, delete rest

### 3.4 What the chain planner produces

Given:
- Input path + detected source format/codec/rate/depth
- Target format, sample rate, bit depth, dither type, resample quality, nyquist transition, SSRC mode
- Preferred backend (ffmpeg/sox/auto)

The chain planner returns:

```
Vec<PlannedCommand> where each PlannedCommand has:
  - tool: ToolIdentifier (known enum variant OR custom binary name)
  - args: Vec<String>
  - input: InputSource (file path or stdin pipe)
  - output: OutputSink (file path or stdout pipe)
  - expected_duration: Option<Duration>
  - description: String
```

Single-step example (FLAC 24/96 → FLAC 16/44.1 with Shibata dither via sox):
```
[
  sox input.flac -b 16 output.flac rate -v 44100 dither -f shibata
]
```

Multi-step example (FLAC 24/96 → FLAC 16/44.1 with SSRC brick-wall + Shibata):
```
[
  ffmpeg -i input.flac -f wav -sample_fmt s32 - | ssrc --rate 44100 --profile long --twopass - temp.wav,
  sox temp.wav -b 16 output.flac dither -f shibata
]
```

DSD example (DSF DSD64 → FLAC 24/88.2 via sox):
```
[
  sox -S input.dsf -b 24 output.flac rate -v 88200
]
```

Passthrough example (FLAC → FLAC, same settings):
```
[]  // empty — orchestrator does fs::copy instead
```

---

## 4. Design constraints

1. **Extensibility for first-class formats/tools:** New materializers (RAR, ZIP, ISO9660), new encoders (Ogg Vorbis, DTS, AC3, rox backend), and new tools (SSRC variants) are added by implementing traits. The orchestrator and chain planner dispatch without knowing the specific format.

2. **User scripts are out of scope.** A separate hook system will call arbitrary user-defined scripts at designated points. The pipeline crate does not handle this. The orchestrator calls hooks after the pipeline produces its output.

3. **The system works today.** This is a refactor, not a rewrite of user-facing behavior. All existing conversion scenarios must continue to work.

4. **The backend command builders work.** FFmpegBuilder and SoxBuilder correctly construct commands. Migrate them, don't rewrite them. Fix the store_md5 bug (ID3v2 on FLAC) during migration.

5. **ToolRunner is the execution boundary.** The pipeline crate produces commands. The main crate's ToolRunner executes them. Progress reporting, cancellation, timeouts — all handled by ToolRunner. The pipeline crate never spawns processes.

6. **Settings flow is the core invariant.** Every setting the user selects in the TUI must reach the command builder that constructs the tool invocation. No information loss at any boundary. This is the bug we're fixing.

7. **One path, not three.** After the redesign, every source type (single file, batch, SACD, CUE, 7z) goes through the same orchestrator → chain planner → ToolRunner flow. Copy mode is a special case of the chain planner returning an empty command list.

8. **Deterministic command construction.** Same ConversionSettings + same source properties → same command list. No randomness, no ambient state, no config file side-reads in the pipeline crate.

---

## 5. Deliverables requested

For each of the three chunks below, produce:

### Chunk 1: Pipeline crate + settings unification

- The `ConversionSettings` struct (single source of truth for all conversion parameters)
- The `AudioFormat` enum (all current + future formats)
- The `DitherType` enum (all variants, no lossy wrappers)
- The encoder trait (or equivalent dispatch mechanism) for extensible format support
- The chain planner's function signature and return type
- How FFmpegBuilder/SoxBuilder are called from the chain planner
- How SSRC is integrated as a chain step
- How passthrough (no-encode) is represented
- The crate's public API surface — every pub type and function

### Chunk 2: Orchestrator unification

- How stages.rs's orchestrator calls the pipeline crate
- How PlannedCommand maps to ToolCommand for execution via ToolRunner
- The ToolBinary changes (SSRC variant, future extensibility)
- How copy mode collapses into the unified path
- How the three pipeline request builder functions (SACD, CUE, 7z) are unified or simplified
- How the legacy convert_with_backend() path is eliminated
- How progress reporting works for multi-step chains (one ToolRunner call per step? or piped?)
- How metadata, ReplayGain, lineage, and rename stages interact with the new encode step
- Error handling: what happens when step 2 of a 3-step chain fails?
- Parallelism: the orchestrator interface must not foreclose intra-album parallelism (encoding multiple tracks concurrently). Currently tracks encode serially — the redesign should make parallel track encoding a future option without requiring interface changes. Inter-album parallelism (multiple albums in parallel via worker pool) already exists in processor.rs and stays.

### Chunk 3: Format pane redesign

- The new FormatState (or replacement) with dynamic rows based on format family
- PCM rows: format, sample rate, bit depth, dither (expanded options), replaygain
- DSD rows: format, DSD rate, 1-bit label, noise shaper, conversion preset
- Resampler pill (sox / ssrc / soxr) and how it feeds into the chain planner
- Auto-dither defaults: Shibata for →16-bit, TPDF for →24-bit, none for no reduction
- The constraint cascade redesign (multi-axis: format family × resampler × bit depth delta)
- Preset schema v3 with new fields
- FormatField navigation (keyboard up/down through dynamic rows)
- How FormatState produces a ConversionSettings for the pipeline crate

For each chunk: struct definitions, trait definitions, function signatures, enum definitions, and a sequenced implementation plan showing which files change and in what order. Include error types where relevant.

Do not produce implementation code. Produce the architecture: types, traits, signatures, data flow, and sequencing.

---

## 6. Additional context

### 6.1 Naming collision: "ConversionSettings" appears THREE times

- `src/config.rs:48` — `ConversionSettings` — user's config.toml preferences (preferred_backend, worker_count, etc.). NOT conversion parameters.
- `src/convert/formats.rs:121` — `ConversionOptions` — TUI pill state mapped to conversion parameters. 23 fields.
- `crates/tonepoet-backend/src/types.rs:102` — `ConversionSettings` — backend command builder input. 23 fields (different 23 from ConversionOptions).

The new pipeline crate's unified settings type should have an unambiguous name to avoid collision with the config.toml struct.

### 6.2 tonepoet-features crate dependency

`crates/tonepoet-features/Cargo.toml` depends on `tonepoet-backend` and imports `ConversionPipeline` (one import, always set to `None` from the main crate — effectively dead). If the backend crate is gutted, this import breaks. The features crate needs a trivial update to remove this dependency or redirect it to the new pipeline crate.

### 6.3 sacd-rs crate

Self-contained SACD ISO parser + DSD extractor. Has its own `OutputFormat::Dsf`/`OutputFormat::Dff` enum for extraction. Does NOT use the main crate's `AudioFormat` or any pipeline types. No changes needed — it's a materializer input, not a pipeline participant.

### 6.4 tonepoet-wizard crate

Legacy TUI wizard. Standalone — no dependencies on backend, features, or pipeline. Has its own format/quality/dither types that are independent. Not affected by this redesign.

---

### 7. Reference files

These files contain domain knowledge that informs the design:

- `docs/pcm-settings.md` — Complete PCM conversion settings reference: sample rates, bit depths, dither algorithms per backend (sox/ffmpeg/ssrc), resampling options, SSRC noise shaping matrix, format constraints
- `docs/SOX_DSD_BASICS.md` — DSD encoding: noise shaper types (CLANS/SDM/CRFB), order selection by DSD rate, trellis optimization, signal level standards
- `docs/dsd-and-pcm-conversion-presets.md` — Auto/Sinc preset definitions for PCM↔DSD conversion, pass-band defaults, FIR parameters, gain compensation tables
- `docs/phase0_sequencing_plan_hardened_ready_for_execution.md` — Existing pipeline rebuild plan (PRs 1-10, 19 invariants) — context for what was already built

---

## 8. Current file locations (for reference, not prescription)

| Component | File | Lines | Status |
|-----------|------|-------|--------|
| Pipeline orchestrator | src/convert/pipeline/stages.rs | 6,184 | KEEP (modify encode step) |
| Pipeline types | src/convert/pipeline/types.rs | 586 | REPLACE |
| ToolRunner + ToolBinary | src/convert/pipeline/tool.rs | 457 | KEEP (add SSRC) |
| Progress system | src/convert/pipeline/progress/ | 2,540 | KEEP |
| Materializers | src/convert/pipeline/materializer_*.rs | 2,258 | KEEP |
| Queue processor | src/convert/processor.rs | 2,251 | SIMPLIFY (one path) |
| ConversionOptions | src/convert/formats.rs | 463 | REPLACE with pipeline crate type |
| DitherType/NyquistTransition | src/convert/simple_wizard.rs | 182 | MOVE to pipeline crate |
| FFmpeg command builder | crates/tonepoet-backend/src/ffmpeg.rs | ~286 | MOVE to pipeline crate |
| Sox command builder | crates/tonepoet-backend/src/sox.rs | ~178 | MOVE to pipeline crate |
| Mapping functions | crates/tonepoet-backend/src/mapping.rs | ~264 | MOVE to pipeline crate |
| Backend ConversionSettings | crates/tonepoet-backend/src/types.rs | ~861 | MOVE/MERGE to pipeline crate |
| Backend pipeline orchestrator | crates/tonepoet-backend/src/pipeline.rs | ~2,300 | DELETE |
| Type translation layer | crates/tonepoet-backend/src/integration.rs | ~398 | DELETE |
| Type translation API | crates/tonepoet-backend/src/integration_api.rs | ~310 | DELETE |
| Metadata tools | crates/tonepoet-backend/src/metadata.rs | ~1,637 | EVALUATE (keep if useful) |
| TUI FormatState | src/tui/app.rs (FormatState) | ~180 | REDESIGN (Chunk 3) |
| TUI format pane draw | src/tui/draw_output.rs | ~160 | REDESIGN (Chunk 3) |
| TUI pills→options bridge | src/tui/convert_actions.rs | 276 | REDESIGN (Chunk 3) |
| TUI presets | src/tui/presets.rs | ~470 | UPDATE (schema v3) |
