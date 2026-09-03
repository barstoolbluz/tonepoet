# Implementation report - silent request divergences

**Date:** 2026-09-03  
**Base:** `main` @ `9562acd`  
**Brief:** `BRIEF_silent_request_divergences_2026-09-03.md`  
**Patch SHA-256:** `09716707125b37a6ec6fb5dc441a6c5107d3552eb5ebf9ee5d82361b8b0c70cb`

## Product decision implemented

For ordinary lossy conversions, when the requested/resolved PCM rate is not accepted directly by Tonepoet's configured encoder, Tonepoet resolves to the **highest directly supported rate at or below the request** and discloses the change. This preserves successful ordinary conversions while ending implicit FFmpeg negotiation.

Album-scoped DSD hard-ceiling NormalizePeak is deliberately asymmetric: it **never adapts the measured carrier rate**. If the configured lossy encoder cannot accept that exact rate directly, planning refuses the conversion in Tonepoet's own words. This applies both before and after runtime album gain is bound.

## Implementation

### 1. One encoder-rate authority

`tonepoet-pipeline/src/mapping.rs` now exposes ordered direct-input sample-rate tables for MP3, AAC, Opus, DTS and AC-3. The existing direct-acceptance predicate is derived from those tables, and ordinary fallback is derived from the same tables.

This is the single source of truth used by:

- TUI concrete rate admission;
- planner fallback;
- hard-ceiling exact-rate refusal;
- SSRC rate-dependent dither validation; and
- the final FFmpeg lossy command's fail-closed rate-pin check.

### 2. TUI and presets

`FormatState::apply_format_constraints` no longer carries duplicated AAC/MP3/Opus/DTS/AC-3 concrete-rate caps. It crosses the existing TUI/pipeline enum boundary through `map_audio_format` and queries the planner table.

Effects over the picker rates are intentional:

- AAC: 176.4 and 192 kHz are disabled; 96 kHz is the maximum selectable concrete rate.
- MP3 and Opus: existing concrete behavior is unchanged.
- DTS and AC-3: valid 44.1 kHz becomes selectable; their historical Source sentinel remains disabled/pinned behavior.
- Ogg remains decode-only and is not routed through an output encoder table.

Constraint clamping now occurs before rate-dependent DSD Wideband admission is recomputed, preventing a stale 176.4/192 kHz selection from leaving Wideband enabled after AAC clamps to 96 kHz.

A preset carrying a now-invalid concrete AAC 192 kHz request uses the existing refused-field mechanism and names `sample_rate` explicitly rather than silently retaining it.

### 3. Planner resolves ordinary lossy rates before processing

The pure planner resolves a lossy encoder-input rate before constructing processing topology:

- PCM Source 192 kHz -> AAC resolves to 96 kHz.
- Explicit PCM 176.4 kHz -> AAC resolves to 96 kHz.
- PCM Source 192 kHz -> MP3 resolves to 48 kHz.
- DSD Source -> AAC resolves the default DSD-to-PCM rate before carrier creation, so the DSD carrier itself is built at the effective supported rate rather than being rebuilt/resampled implicitly inside FFmpeg.
- A valid global rate below the target encoder's minimum fails with a Tonepoet planning diagnostic instead of falling through to a raw encoder error.

Rate-dependent SSRC dither validation is evaluated against the effective ordinary-path rate. Hard-ceiling requests defer unsupported-rate diagnosis to request planning so an unrelated dither-table error cannot mask the hard-ceiling refusal.

Mixed DSD/PCM album jobs are handled at the real authority boundary: only DSD-derived participants are exact-rate hard-ceiling participants. A normal PCM member that happens to carry album-gain settings still follows ordinary lossy fallback, matching the existing processor/plan-bridge exclusion of non-DSD tracks from album gain.

### 4. FFmpeg lossy encode is fail-closed

Built-in lossy FFmpeg encode commands now require an explicit resolved `target_rate_hz` and always emit `-ar`.

The command builder independently rejects:

- a missing rate pin; or
- a rate the configured encoder table does not accept directly.

Runtime DSD album gain retains its stronger measured-carrier equality check. FFmpeg is therefore never given permission to choose or silently resample the final lossy rate.

### 5. User-visible and durable divergence disclosure

Before tool execution, successful output planning emits a plain progress/log disclosure for ordinary rate fallback, for example that AAC cannot encode 192 kHz directly and the conversion will use 96 kHz instead.

Hard-ceiling unsupported-rate requests are refused during planning with an actionable Tonepoet diagnostic; they are not adapted.

The conversion summary also records ordinary requested -> effective sample-rate divergence. Settings/sample-rate summary text and canonical output naming use the effective ordinary-path rate so logs, `%SAMPLERATE%`-derived names and the actual plan do not contradict one another.

### 6. Hard-ceiling headroom disclosure without changing the proof

No true-peak or reserve implementation was changed.

Disclosure calls the existing `album_gain_terminal_bound` and reports the deterministic post-gain terminal realization reserve. The pre-conversion progress message is emitted for reserves >= 0.001 dB; smaller reserves remain recorded in the durable conversion summary without adding noisy UI warnings.

Regression values at 44.1 kHz include:

- Int16, no dither: ~0.000542098 dB
- Int16, TPDF: ~0.001626379 dB
- Int16, Gesemann: ~0.023884023 dB
- Int16, Shibata: ~0.091549024 dB
- Int16, High-Shibata: ~0.169689406 dB
- Int24, High-Shibata: ~0.000656449 dB

These are disclosure tests only; the terminal support and finite-stream endpoint proof remain untouched.

## Regression coverage added

Coverage was added for:

- picker/planner table coherence and AAC 192 -> 96 clamping;
- DTS/AC-3 44.1 kHz admission and preserved Opus Source pin behavior;
- invalid concrete AAC preset refusal;
- ordered fallback-table behavior;
- PCM Source and explicit-rate fallback planning;
- DSD carrier construction at the resolved ordinary lossy rate;
- pre-runtime hard-ceiling exact-rate refusal;
- mixed PCM tracks carrying album settings;
- below-encoder-minimum Tonepoet diagnostics;
- final FFmpeg missing/unsupported rate-pin defense;
- SSRC dither validation at the effective rate;
- pre-conversion and durable rate-divergence reporting;
- hard-ceiling refusal vs fallback disclosure;
- headroom disclosure threshold behavior; and
- the existing terminal-bound reserve values, including 16-bit High-Shibata at ~0.169689 dB.

## Scope / invariants

Exactly eight existing source files differ from the supplied baseline:

1. `src/convert/pipeline/stages.rs`
2. `src/tui/app.rs`
3. `src/tui/convert_actions.rs`
4. `src/tui/presets.rs`
5. `tonepoet-pipeline/src/mapping.rs`
6. `tonepoet-pipeline/src/plan.rs`
7. `tonepoet-pipeline/src/plugins.rs`
8. `tonepoet-pipeline/src/settings.rs`

`crates/tonepoet-true-peak` is byte-identical to the supplied baseline. No process spawning, filesystem I/O or interactive behavior was added to the pure planner. No process-global test mutation, F-key binding, emoji or decorative Unicode was added by this change.

## Validation status

The implementation container has no `nix`, `cargo`, `rustc`, `rustfmt` or project audio tools, matching the brief's stated environment constraint. Therefore **no compile/test gate is claimed**.

Static checks performed here:

- content comparison confirms exactly eight modified source files;
- true-peak subtree comparison: identical;
- unified-diff whitespace/error check: clean;
- lexical delimiter scan over all eight modified Rust files: balanced;
- no added forbidden process/interactive imports in `tonepoet-pipeline`;
- no added process-global test-state mutation tokens;
- no added F-key references or non-ASCII UI text;
- production `EncodeLossy` topology audited so the final command is rate-pinned; remaining `target_rate_hz: None` lossy construction is deliberate defensive/container-validation test coverage, not a production route;
- requested/effective-rate dependency sweep covered DSD reconstruction profile admission, SSRC dither, post-encode sample expectations, ReplayGain equivalence, output naming and conversion summaries.

## Operator acceptance gate

Run from the repository root in the project's required Nix development shell:

```bash
nix develop --extra-experimental-features 'nix-command flakes'
cargo check
cargo test --workspace
```

Per `CLAUDE.md`, do not substitute system Rust and do not use plain `cargo test` as the workspace gate.
