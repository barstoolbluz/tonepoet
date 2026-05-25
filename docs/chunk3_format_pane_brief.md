# Chunk 3: Format Pane Redesign

## For: Reasoning model (GPT Pro)
## Project: tonepoet — CLI + TUI audio conversion toolkit
## Language: Rust (edition 2021, async via Tokio)
## Quality bar: Rigor, correctness, robustness, idempotency, performance (in that order).
## Prerequisites: Chunks 1, 2, and 2.1.1-2.1.3 are integrated. 788 tests pass (0 failures, 1 ignored). The tonepoet-pipeline crate owns the unified `PipelineSettings` type. `ConversionOptions.pipeline_settings: Option<PipelineSettings>` exists but is never populated by the TUI.

---

## 1. What this chunk does

Wire the TUI format pane to produce `PipelineSettings` instead of (or in addition to) legacy `ConversionOptions` fields. Redesign `FormatState` with dynamic rows that change based on format family (PCM vs DSD), add a resampler pill, expand dither options, and implement auto-dither defaults based on source/target bit depth.

After this chunk, every user setting selected in the TUI reaches the pipeline crate's command builders with zero information loss. The 2 static audit test failures (`PipelineSettings::default()` in main.rs and command.rs) resolve because real settings are constructed from TUI state.

---

## 2. Current state

### 2.1 FormatState (src/tui/app.rs:587-657)

Five fixed pill rows:
1. **format**: FLAC, Opus, AAC, MP3, ALAC, WAV, WavPack, DSF, DFF
2. **sample rate**: 44.1-768 kHz + DSD64-DSD512 (all in one row, constraint cascade enables/disables)
3. **bit depth**: 16, 24, 32, 32f, 64f
4. **dither**: TPDF, none, shaped (only 3 of 11 variants)
5. **replaygain**: album, track, both, off

Navigation: `FormatField` enum cycles through 5 fixed rows via up/down keys.

### 2.2 pills_to_options() (src/tui/convert_actions.rs:16-101)

Builds `ConversionOptions` from pills. Sets legacy fields (output_format, quality, target_sample_rate, target_bit_depth, dither_type, replaygain_mode). Does NOT set `pipeline_settings`.

### 2.3 Presets (src/tui/presets.rs)

`TuiPreset` stores format/rate/depth/dither/replaygain as strings. Version 2 schema. No PipelineSettings field. No resampler, no DSD noise shaper, no codec-specific settings.

### 2.4 What's missing

- `pills_to_options()` never populates `pipeline_settings` — settings are lost at the orchestrator boundary
- Only 3 of 11 dither algorithms exposed (TPDF, None, Shibata)
- No resampler pill (sox/ssrc/soxr)
- No DSD-specific rows (noise shaper, conversion preset)
- Bit depth row shows for DSD formats (should be hidden — DSD is always 1-bit)
- Dither row shows for DSD formats (should show noise shaper instead)
- ReplayGain row shows for DSD formats (not applicable — should show conversion preset)
- No auto-dither defaults (should auto-select Shibata for →16-bit, TPDF for →24-bit)
- Presets don't store PipelineSettings fields
- FormatField navigation is fixed at 5 rows — can't adapt to dynamic PCM/DSD row sets

---

## 3. Target state

### 3.1 Dynamic rows based on format family

**When a PCM format is selected** (FLAC, WAV, AIFF, WavPack, MP3, AAC, Opus, ALAC):

| Row | Label | Options |
|-----|-------|---------|
| 1 | format | FLAC Opus AAC MP3 ALAC WAV WavPack DSF DFF |
| 2 | sample rate | 44.1 48 88.2 96 176.4 192 352.8 384 705.6 768 (kHz) |
| 3 | bit depth | 16 24 32 32f 64f (per format constraints) |
| 4 | resampler | sox ssrc soxr |
| 5 | dither | TPDF none Shibata Low-Shibata High-Shibata Gesemann Lipshitz (expanded) |
| 6 | replaygain | album track both off |

**When a DSD format is selected** (DSF, DFF):

| Row | Label | Options |
|-----|-------|---------|
| 1 | format | FLAC Opus AAC MP3 ALAC WAV WavPack DSF DFF |
| 2 | DSD rate | DSD64 DSD128 DSD256 DSD512 |
| 3 | bit depth | 1-bit (static label, not a pill) |
| 4 | noise shaper | CLANS SDM CRFB |
| 5 | modulator order | 4th 5th 6th 7th 8th |
| 6 | conversion preset | Auto Sinc |

Row 1 (format) is always the same. Rows 2-6 change semantically based on the format family.

### 3.2 Resampler pill (new, PCM only)

A new pill row between bit depth and dither:
- **sox**: SoX native resampler (`rate -v`, `-h`, etc.)
- **ssrc**: SSRC brick-wall resampler
- **soxr**: libsoxr via FFmpeg (`aresample=resampler=soxr`)

When SSRC is selected, `NyquistTransition::BrickWall` is set in PipelineSettings. When sox/soxr is selected, the existing Nyquist transition preference applies.

### 3.3 Expanded dither options (PCM only)

Show more of the 11 DitherType variants as pills. The basic view shows the most common:
- TPDF, none, Shibata, Gesemann, Lipshitz

Advanced view (when advanced_open is true) could show all 11.

### 3.4 Auto-dither defaults

When the user changes bit depth, auto-select the appropriate dither:
- Target ≤16-bit from higher source: **Shibata**
- Target 24-bit from higher source: **TPDF**
- No bit depth reduction: **None**

This is a default — the user can override by manually selecting a dither pill.

### 3.5 FormatState → PipelineSettings bridge

A new function (or extension of `pills_to_options`) that constructs `PipelineSettings` from `FormatState`:

```rust
pub fn format_state_to_pipeline_settings(
    format: &FormatState,
    /* possibly source info for auto-dither */
) -> PipelineSettings
```

Mapping:
- `format.format` → `PipelineSettings.target_format` (map main crate AudioFormat → pipeline AudioFormat)
- `format.sample_rate` → `PipelineSettings.target_sample_rate`:
  - PCM rates → `RateTarget::PcmHz(rate)`
  - DSD rates → `RateTarget::Dsd(DsdRate::from_hz(rate))`
- `format.bit_depth` → `PipelineSettings.target_bit_depth`:
  - `BitDepthChoice::Int16` → `BitDepthTarget::Pcm(PcmBitDepth::Int16)`, etc.
  - DSD format → `BitDepthTarget::Source` (ignored for DSD)
- `format.dither` → `PipelineSettings.dither_type` (direct mapping — both use DitherType)
- `format.replaygain` → `PipelineSettings.replay_gain.mode`:
  - Album → `Some(ReplayGainMode::Album)`, Track → `Some(ReplayGainMode::Track)`, Both → `Some(ReplayGainMode::Both)`, Off → `None`
- Resampler pill → `PipelineSettings.preferred_tool` + `PipelineSettings.nyquist_transition`:
  - sox → `PreferredTool::Sox`, `NyquistTransition::Gentle`
  - ssrc → `PreferredTool::Ssrc`, `NyquistTransition::BrickWall`
  - soxr → `PreferredTool::Ffmpeg`, `NyquistTransition::Gentle`
- DSD noise shaper → `PipelineSettings.dsd.noise_shaper`
- DSD modulator order → `PipelineSettings.dsd.modulator_order`
- DSD conversion preset → `PipelineSettings.dsd.pcm_to_dsd_filter`

Format-specific settings (FLAC compression, MP3 bitrate, AAC profile, Opus complexity) use sensible defaults. These can become advanced settings later.

### 3.6 Preset schema v3

Add new fields to `TuiPreset`:
```
resampler: String          // "sox", "ssrc", "soxr"
noise_shaper: Option<String>    // "clans", "sdm", "crfb" (DSD only)
modulator_order: Option<u8>     // 4-8 (DSD only)
dsd_filter_preset: Option<String> // "auto", "sinc" (DSD only)
```

Use `serde(default)` for backward compatibility — v2 presets load with defaults for new fields.

### 3.7 FormatField navigation redesign

Replace the fixed 5-variant `FormatField` enum with a dynamic approach:

```rust
pub enum FormatField {
    Format,
    // PCM rows
    SampleRate,
    BitDepth,
    Resampler,
    Dither,
    ReplayGain,
    // DSD rows
    DsdRate,
    NoiseShaper,
    ModulatorOrder,
    ConversionPreset,
}
```

`next()` and `prev()` skip rows that aren't visible for the current format family.

---

## 4. Design constraints

1. **The tonepoet-pipeline crate is not modified.** All work is in the main crate.
2. **`pills_to_options()` must set `pipeline_settings: Some(...)`.** This is the primary deliverable — without it, settings don't reach the backend.
3. **Backward compatible presets.** v2 presets load with defaults for new fields. v3 presets save all fields.
4. **Existing tests must pass.** The 788 passing tests stay passing. The 2 static audit failures (PipelineSettings::default() in main.rs and command.rs) should resolve.
5. **The format pane height stays at 10 rows.** The same 10-row layout, but rows 2-6 change content based on format family.
6. **Mouse click registration adapts.** Button registration already uses dynamic y-offsets (from Chunk 2 work). New pill rows need button variants.

---

## 5. Deliverables

1. **Dynamic FormatState** — FormatField that adapts to PCM vs DSD. New pill states for resampler (PCM), noise shaper (DSD), modulator order (DSD), conversion preset (DSD).

2. **format_state_to_pipeline_settings()** — the bridge function. Maps every FormatState pill to the corresponding PipelineSettings field. Validates the result.

3. **pills_to_options() update** — calls format_state_to_pipeline_settings() and sets `options.pipeline_settings = Some(settings)`.

4. **Auto-dither logic** — when bit depth changes, auto-select appropriate dither. User override preserved.

5. **Expanded dither pills** — show at least 5 of the 11 DitherType variants in basic view.

6. **Resampler pill** — sox / ssrc / soxr. Maps to PreferredTool + NyquistTransition in PipelineSettings.

7. **DSD-specific rows** — noise shaper, modulator order, conversion preset. Only visible when DSF/DFF selected.

8. **Constraint cascade update** — apply_format_constraints() updated for resampler, DSD rows, expanded dither.

9. **Format pane rendering** — draw_output.rs updated for dynamic rows. Renders PCM or DSD row set based on selected format.

10. **Preset schema v3** — new fields, backward compatible, round-trip tested.

11. **Button registration** — new TuiButton variants for resampler pill, noise shaper pill, modulator order pill, conversion preset pill.

12. **Keyboard navigation** — FormatField::next()/prev() skip invisible rows.

13. **Static audit resolution** — main.rs and command.rs use format_state_to_pipeline_settings() instead of PipelineSettings::default().

14. **Tests** — format_state_to_pipeline_settings round-trip, auto-dither behavior, constraint cascade for all format families, preset v3 round-trip.

---

## 6. Code files the reasoning model needs

1. **src/tui/app.rs** — FormatState, FormatField, BitDepthChoice, apply_format_constraints(), PillState usage
2. **src/tui/convert_actions.rs** — pills_to_options() (the function to modify)
3. **src/tui/presets.rs** — TuiPreset, from_pill_state(), apply_to_pills(), parse functions
4. **src/tui/draw_output.rs** — format pane rendering
5. **src/tui/pill.rs** — PillState<T> (the generic pill widget)
6. **src/tui/button_map.rs** — TuiButton enum (needs new variants)
7. **src/tui/keybindings.rs** — format pane key handling (relevant excerpt only)
8. **tonepoet-pipeline/src/settings.rs** — PipelineSettings target type
9. **tonepoet-pipeline/src/enums.rs** — all pipeline enums
10. **src/convert/formats.rs** — ConversionOptions with pipeline_settings field
11. **docs/pcm-settings.md** — dither/resampler reference
12. **docs/SOX_DSD_BASICS.md** — DSD noise shaper reference
13. **docs/dsd-and-pcm-conversion-presets.md** — DSD conversion preset reference

---

## 7. Acceptance criteria

- [ ] `pills_to_options()` sets `pipeline_settings: Some(settings)` — no more `None`
- [ ] Every FormatState pill maps to a specific PipelineSettings field
- [ ] DSD format selection shows DSD-specific rows (rate, noise shaper, modulator order, conversion preset)
- [ ] PCM format selection shows PCM-specific rows (rate, depth, resampler, dither, replaygain)
- [ ] Resampler pill exists with sox/ssrc/soxr options
- [ ] Dither pill shows at least 5 algorithms (TPDF, none, Shibata, Gesemann, Lipshitz)
- [ ] Auto-dither: Shibata for →16-bit, TPDF for →24-bit, None for no reduction
- [ ] Preset v3 saves and loads new fields (resampler, DSD settings)
- [ ] v2 presets load with defaults for new fields (backward compatible)
- [ ] Static audit tests for PipelineSettings::default() resolve (main.rs, command.rs use real settings)
- [ ] FormatField navigation skips invisible rows
- [ ] Mouse clicks work on all new pill rows
- [ ] All existing 788 tests still pass
- [ ] New tests for bridge function, auto-dither, constraint cascade, preset round-trip
