# Chunk 1: Pipeline Crate + Settings Unification

## For: Reasoning model (GPT Pro)
## Project: tonepoet — CLI + TUI audio conversion toolkit
## Language: Rust (edition 2021, async via Tokio)
## Quality bar: Rigor, correctness, robustness, idempotency, performance (in that order). Deterministic command construction (same inputs → same outputs). Idempotent where applicable: re-running the same conversion produces identical output; re-running after interruption does not leave corrupt state or orphan files.

---

## 1. What tonepoet does

tonepoet converts audio files between formats (FLAC, WAV, AIFF, WavPack, MP3, AAC, Opus, ALAC, DSF, DFF, and more). It extracts multi-track sources (SACD ISOs, CUE+image files, 7z archives), applies sample rate conversion, bit depth conversion, dithering, noise shaping, ReplayGain, metadata preservation, and file renaming.

The conversion pipeline runs external tools (ffmpeg, sox, ssrc, loudgain, metaflac, opustags, etc.) by constructing command-line arguments from a settings struct. This chunk designs the **single settings type** and the **chain planner** that replaces the current fragmented architecture.

---

## 2. The problem: type duplication and settings loss

### 2.1 Three parallel type hierarchies

The same concepts are defined multiple times across two crates:

| Concept | Main crate (simple_wizard.rs) | Main crate (pipeline/types.rs) | Backend crate (types.rs) | Backend crate (integration.rs) |
|---------|------------------------------|-------------------------------|--------------------------|-------------------------------|
| Dither algorithm | `DitherType` (11 variants) | `DitherPolicy` (3 variants: Auto/Off/On) | `DitherType` (9 variants) | `MainDitherType` (11 variants) |
| Audio format | `AudioFormat` (10 variants) | — | `AudioFormat` (8 variants) | `MainAudioFormat` (8 variants) |
| Nyquist transition | `NyquistTransition` (3 variants) | — | `NyquistTransition` (5 variants) | `MainNyquistTransition` (3 variants) |
| AAC profile | `AacProfile` (3 variants) | — | `AacProfile` (4 variants) | `MainAacProfile` (3 variants) |
| ReplayGain mode | `ReplayGainMode` (3 variants) | — | `ReplayGainMode` (3 variants) | `MainReplayGainMode` (3 variants) |
| Settings | `ConversionOptions` (23 fields) | `EncodeOptions` (4 fields) | `ConversionSettings` (23 fields) | `MainConversionOptions` (14 fields) |

`DitherPolicy` is particularly harmful — it collapses 11 specific algorithms into 3 values (Auto/Off/On), discarding the user's choice.

### 2.2 Settings flow gap

The pipeline path (for SACD, CUE, 7z sources) calls `backend_settings()` which populates only 5 of ~25 fields in `BackendConversionSettings`:
- format, compression_level, bitrate, mp3_mode, dither (as Auto — always) ✓
- sample_rate, bit_depth, resample_quality, nyquist_transition, dither_type, ssrc_insane_mode ✗ NOT SET

The legacy single-file path maps ALL fields correctly via `create_backend_conversion_item()` (~180 lines of boilerplate type translation).

### 2.3 Naming collision

"ConversionSettings" appears THREE times:
- `src/config.rs:48` — user's config.toml preferences (preferred_backend, worker_count, etc.). NOT conversion parameters.
- `src/convert/formats.rs:121` — `ConversionOptions` — TUI pill state mapped to conversion parameters.
- `crates/tonepoet-backend/src/types.rs:102` — `ConversionSettings` — backend command builder input.

The new pipeline crate's unified type needs an unambiguous name.

### 2.4 Verified facts

All verified by reading the actual code with file paths and line numbers confirmed:

- PipelineRequest has NO fields for target_sample_rate or target_bit_depth (pipeline/types.rs:56-78)
- DitherPolicy::Auto is hardcoded in 5 locations (processor.rs:1055,1183,1378,1658; command.rs:3910)
- backend_settings() extracts 5 fields from PipelineRequest (stages.rs:4686-4700)
- The legacy single-file path maps ALL ConversionOptions fields (processor.rs:2120-2177)
- Backend crate's PipelineBuilder is NEVER called from the main crate (confirmed via grep)
- The TUI has zero imports from tonepoet-backend (confirmed via grep across src/tui/)
- SSRC is a build dependency (flake.nix) with full command construction in backend pipeline.rs:594-694

---

## 3. Reference material (all code in the zip is reference, nothing is reused verbatim)

Everything in the new crate is designed from scratch. The existing code shows:

- **What arguments ffmpeg/sox expect** — `ffmpeg.rs` and `sox.rs` show the command-line patterns for each format, sample rate, bit depth, dither, etc. Use these to understand what the new tool plugins must produce.
- **What parameter mappings exist** — `mapping.rs` shows how dither types, resample quality, Nyquist transition, and SSRC profiles translate to tool-specific flags. Use these to understand the mapping domain.
- **What fields a settings type needs** — `types.rs` (backend) has 23 fields covering most conversion parameters. Has dead fields (`name`, `version`, `selected_quality`) and lacks DSD fields. Use as a starting point for understanding scope.
- **What enum variants exist** — `simple_wizard.rs` (main crate) and `types.rs` (backend) show the current variant sets for DitherType, NyquistTransition, AudioFormat, etc. The new crate unifies and extends these.
- **What the duplication looks like** — `integration.rs` shows the Main* type hierarchy that exists solely to bridge two type systems. This goes away entirely with unified types.

---

## 4. What's broken (REPLACE)

- **`DitherPolicy` enum** (pipeline/types.rs) — lossy 3-value wrapper. Delete, use DitherType everywhere.
- **`EncodeOptions`** (pipeline/types.rs) — 4 fields. Lacks sample_rate, bit_depth, dither_type, resample_quality, nyquist_transition, ssrc_insane_mode, preferred_backend. Replace with the unified settings type.
- **`integration.rs` Main* type hierarchy** (~398 lines) — exists solely to bridge two type systems. Delete when there's one type system.
- **`create_backend_conversion_item()`** (processor.rs, ~180 lines) — boilerplate mapping between duplicate types. Delete.
- **`backend_settings()`** (stages.rs) — populates 5 of 23 fields. Replace with direct use of unified type.
- **store_md5 bug** — ffmpeg.rs:276-281 writes ID3v2 tags to FLAC files (should be vorbis comments). Fix during migration.
- **Dead fields in ConversionSettings** — `name`, `version`, `selected_quality` are never read by any command builder.

---

## 5. Target: new crate `tonepoet-pipeline`

A new crate that owns:

1. **The single settings type** — one struct, no duplicates. Replaces ConversionOptions + EncodeOptions + BackendConversionSettings + MainConversionOptions.
2. **The AudioFormat enum** — one definition with all variants (current 10 + future extensibility).
3. **The DitherType enum** — one definition with all 11 variants, no lossy wrappers.
4. **All supporting enums** — NyquistTransition, AacProfile, ReplayGainMode, Mp3Mode, OpusContentType, etc. One definition each.
5. **Command builders** — FFmpeg, Sox, SSRC tool plugins, designed from scratch as trait implementations (existing ffmpeg.rs/sox.rs are reference for argument patterns).
6. **Chain planner** — given settings + source info, produces an ordered list of commands.
7. **Mapping functions** — parameter-to-argument mappings (existing mapping.rs is reference for the mapping domain).
8. **The encoder trait** — extensible interface for adding new output formats/tools.

The pipeline crate is **pure** — it takes settings in, produces commands out. It does NOT execute anything, does NOT know about ToolRunner, does NOT know about the TUI, does NOT spawn processes.

### 5.1 What the chain planner produces

Given:
- Input path + detected source format/codec/rate/depth/channels
- Target settings (format, sample rate, bit depth, dither type, resample quality, nyquist transition, SSRC mode, preferred backend)

Returns:

```
Vec<PlannedCommand> where each PlannedCommand has:
  - tool: ToolIdentifier (known enum variant OR custom binary name)
  - args: Vec<String>
  - input: InputSource (file path or stdin pipe)
  - output: OutputSink (file path or stdout pipe)
  - expected_duration: Option<Duration>
  - description: String
```

Examples:

Single-step (FLAC 24/96 → FLAC 16/44.1 with Shibata dither via sox):
```
[ sox input.flac -b 16 output.flac rate -v 44100 dither -f shibata ]
```

Multi-step (FLAC 24/96 → FLAC 16/44.1 with SSRC brick-wall + Shibata):
```
[ ffmpeg -i input.flac -f wav -sample_fmt s32 - | ssrc --rate 44100 --profile long --twopass - temp.wav,
  sox temp.wav -b 16 output.flac dither -f shibata ]
```

DSD (DSF DSD64 → FLAC 24/88.2 via sox):
```
[ sox -S input.dsf -b 24 output.flac rate -v 88200 ]
```

Passthrough (FLAC → FLAC, same settings):
```
[]  // empty — caller does fs::copy
```

---

## 6. Design constraints

1. **Every tool is a plugin.** ffmpeg, sox, and ssrc are not special-cased — they each implement the same trait as any future tool (rox, custom resamplers, etc.). Adding a new tool means implementing the trait and registering it. The chain planner dispatches through the trait, never hardcoding specific tools. This ensures adding or refactoring tools is the same gesture regardless of whether the tool is "built-in" or new.

2. **Pure crate.** No process spawning, no filesystem I/O, no async. Takes settings + source info in, produces command descriptions out. The caller (main crate's orchestrator) handles execution.

3. **Settings flow is the core invariant.** Every field in the settings type must be consumable by at least one command builder. No dead fields. No information loss at any boundary.

4. **Deterministic command construction.** Same settings + same source properties → same command list. No randomness, no ambient state, no config file reads inside the crate.

5. **The existing backend command builders are reference, not gospel.** FFmpegBuilder and SoxBuilder in the zip show what ffmpeg/sox arguments look like for each conversion scenario. Use them as reference for argument construction patterns. But design new builders from scratch as trait implementations within the plugin architecture — the existing builders predate the trait system, have known bugs (store_md5), and lack DSD encoding support (sox sdm commands).

6. **DSD support.** The settings type must support DSD-specific parameters: noise shaper type (CLANS/SDM/CRFB), modulator order, trellis parameters. The chain planner must handle PCM→DSD (via sox sdm) and DSD→PCM (via sox rate) conversions. See `docs/SOX_DSD_BASICS.md` and `docs/dsd-and-pcm-conversion-presets.md` for domain knowledge.

7. **SSRC integration.** The chain planner must support SSRC as a chain step for brick-wall resampling. SSRC is a separate binary (`ssrc`) with its own argument format. See the existing `build_ssrc_command()` in `crates/tonepoet-backend/src/pipeline.rs:594-694` (not included in zip — the mapping functions in `mapping.rs` cover the parameter mappings).

8. **Idempotency.** The chain planner is a pure function — calling it twice with the same inputs produces the same output. No side effects.

---

## 7. Deliverables

Produce the architecture for the `tonepoet-pipeline` crate:

1. **The unified settings struct** — single source of truth for all conversion parameters. Name it to avoid collision with `config.rs::ConversionSettings`. Include all fields needed for PCM and DSD conversion paths.

2. **All enums** — AudioFormat (all current + DSF/DFF + extensibility), DitherType (all 11 variants), NyquistTransition, AacProfile, Mp3Mode, ReplayGainMode, OpusContentType, plus any new DSD-specific enums (NoiseShaper, ModulatorOrder, etc.).

3. **Source info struct** — what the chain planner needs to know about the input (format, codec, sample rate, bit depth, channels, duration). This is read-only input to the planner.

4. **The encoder trait** (or equivalent dispatch mechanism) — how new output formats register their command-building logic.

5. **The chain planner function signature and return type** — `PlannedCommand` struct, `plan_conversion()` function.

6. **How ffmpeg/sox/ssrc tool plugins integrate** — each implements the tool trait. Design the builders from scratch (existing ffmpeg.rs/sox.rs are argument pattern reference only).

7. **How SSRC integrates** as a chain step.

8. **How passthrough (no-encode) is represented** — empty command list? Explicit variant?

9. **The crate's complete public API surface** — every `pub` type, function, and trait.

10. **Error types** — what can go wrong during planning (unsupported format combination, invalid settings, etc.).

For each item: struct definitions, trait definitions, function signatures, enum definitions. Include doc comments that explain the purpose.

Do not produce implementation code for the command builders (they're migrated from the backend crate). Produce the architecture: types, traits, signatures, data flow.

---

## 8. Reference files included in zip

- `docs/pcm-settings.md` — Complete PCM settings reference: sample rates, bit depths, dither algorithms per backend (sox/ffmpeg/ssrc), SSRC noise shaping matrix, format constraints
- `docs/SOX_DSD_BASICS.md` — DSD encoding: noise shaper types, order selection by rate, trellis optimization
- `docs/dsd-and-pcm-conversion-presets.md` — Auto/Sinc preset definitions for PCM↔DSD, pass-band defaults, FIR parameters

## 9. Code files included in zip

| File | Lines | What it contains | Status |
|------|-------|-----------------|--------|
| src/convert/formats.rs | 463 | ConversionOptions, AudioFormat (10 variants), QualitySettings | REFERENCE — shows current settings scope |
| src/convert/simple_wizard.rs | 182 | DitherType (11 variants), NyquistTransition, ReplayGainMode | REFERENCE — shows enum variants to unify |
| src/convert/pipeline/types.rs | 586 | PipelineRequest, EncodeOptions (4 fields), DitherPolicy | REFERENCE — shows the broken interface |
| crates/tonepoet-backend/src/types.rs | 861 | ConversionSettings (23 fields), AudioFormat (8), DitherType (9), all enums | REFERENCE — closest to target settings shape |
| crates/tonepoet-backend/src/ffmpeg.rs | 286 | FFmpegBuilder::build() — constructs ffmpeg commands | REFERENCE — shows ffmpeg argument patterns |
| crates/tonepoet-backend/src/sox.rs | 178 | SoxBuilder::build() — constructs sox commands | REFERENCE — shows sox argument patterns |
| crates/tonepoet-backend/src/mapping.rs | 264 | Parameter mapping functions (dither→args, quality→flags, SSRC profiles) | REFERENCE — shows parameter mapping domain |
| crates/tonepoet-backend/src/integration.rs | 398 | Main* type hierarchy — boilerplate type translation | REFERENCE — shows the duplication to eliminate |
| crates/tonepoet-backend/src/lib.rs | 121 | Current backend crate public API surface | REFERENCE — shows current crate boundary |
