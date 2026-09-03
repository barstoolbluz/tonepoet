# Implementation report - lossy rate fallback corrective R2

**Date:** 2026-09-03  
**Base:** `main` @ `9562acd`, with the silent-request-divergences delivery applied  
**Brief:** `BRIEF_lossy_rate_fallback_corrective_2026-09-03.md`  
**Source patch:** `PATCH_lossy_rate_fallback_corrective_R2_2026-09-03.diff`  
**Patch SHA-256:** `3e112a629306e471a612662cb63d798fe493ca25aebe4dca18ae11c13daa655e`

## Product decision implemented

Ordinary lossy rate resolution now preserves source/request bandwidth whenever the configured
encoder has a higher representable rate available:

- an exact rate remains exact;
- a request below or between configured rates resolves **upward** to the smallest usable rate;
- only a request above the format maximum resolves **downward** to that maximum.

Opus is modeled separately at the same authority boundary: its only rate-stable encoder-boundary
rate is 48 kHz. Lower libopus input modes are not alternate output sample rates; they are
band-limited inputs to a 48 kHz Opus stream. Tonepoet therefore resolves every ordinary Opus
request to 48 kHz and performs any required conversion before the encoder.

Album-scoped DSD hard-ceiling gain remains deliberately asymmetric and unchanged in policy:
unsupported measured carrier rates are refused rather than adapted.

## Implementation

### 1. The single mapping authority now expresses rate-stable encoder boundaries

`tonepoet-pipeline/src/mapping.rs` keeps one table authority for MP3, AAC, Opus, DTS and AC-3,
but clarifies what the table means.

- MP3, AAC, DTS and AC-3 retain their actual encoder/output-rate cells.
- Opus exposes only `48_000` as a direct rate-stable boundary.
- `ffmpeg_lossy_encoder_rate_for_request` replaces the old downward-only helper.
- Binary-search resolution chooses the first configured cell at or above the request, falling
  back to the maximum only when the request is above all cells.

Regression coverage pins:

- AAC 192/176.4 -> 96 kHz;
- AAC 50 -> 64 kHz;
- MP3 96 -> 48 kHz;
- MP3 45 -> 48 kHz;
- AC-3 22.05 -> 32 kHz;
- Opus 8/44.1/192 -> 48 kHz; and
- exact cells remain exact.

This keeps the TUI, planner, hard-ceiling admission, SSRC validation and final FFmpeg builder on
the same rate authority.

### 2. Planner resolution now preserves bandwidth below/between cells

`tonepoet-pipeline/src/plan.rs` uses the new resolver for ordinary lossy requests.

New planner regressions pin the two brief-critical cases:

- PCM Source 44.1 kHz -> Opus resolves to a 48 kHz `EncodeLossy` step with processing enabled.
- PCM Source 22.05 kHz -> AC-3 resolves upward to 32 kHz instead of being refused.

The existing above-maximum AAC/MP3 behavior remains downward and the DSD hard-ceiling
exact-rate refusal branch is unchanged.

`tonepoet-pipeline/src/settings.rs` uses the same effective-rate resolver for rate-dependent SSRC
dither validation, so validation follows the rate the planner will actually deliver.

### 3. FFmpeg ordinary rate changes are explicit Tonepoet SoXR processing

The prior delivery correctly pinned every built-in lossy FFmpeg command with `-ar`, but for an
ordinary rate change that still left FFmpeg to realize the conversion implicitly.

`tonepoet-pipeline/src/plugins.rs` now emits an explicit

`aresample=resampler=soxr:out_sample_rate=...`

filter whenever a direct FFmpeg lossy encode has an ordinary source-to-encoder rate change.
The filter uses the same configured SoXR precision, cutoff, Chebyshev and phase settings as the
existing FFmpeg PCM resampler path; the common option construction is factored into one helper.

The final `-ar` pin remains mandatory and fail-closed. It now confirms the already-explicit filter
output rate instead of being the mechanism that silently causes the resample.

No lossy dither or sample-format behavior was added. Existing runtime album-gain ordering is
preserved (`volume` before `aresample` when both are legitimately present).

The hard-ceiling path is protected by a regression assertion that a proved carrier whose rate
already equals the encoder boundary receives the gain and rate pin but **no** `aresample` filter.

### 4. CUE streaming remains direct without broadening admission

The existing CUE Phase-1 fast path previously rejected every planned FFmpeg `-ar`. Once the prior
delivery made `-ar` mandatory for lossy FFmpeg output, that necessarily rejected the lossy direct
route; the unchanged regression happened to stop first at Opus.

`src/convert/pipeline/track_executor.rs` now admits lossy FFmpeg Phase-1 plans only when:

- the target is a built-in lossy format;
- dither is disabled as required by this path;
- exactly one output `-ar` is present and equals the shared mapping authority's resolved rate; and
- when a rate change is needed, exactly one `-af` is present and is byte-for-byte the planner's
  configured SoXR rate-conversion shape for that request.

Unknown filters, additional `-af` stages, `-filter_complex`, mismatched pins, and all other DSP
shapes continue to fail closed to the established file-backed path.

This preserves the fast direct FFmpeg route for Auto Opus 44.1 -> 48 kHz without introducing a
second resampler process.

### 5. Disclosure follows the corrected policy

`src/convert/pipeline/stages.rs` now resolves requested/effective ordinary lossy rates through the
same corrected helper for:

- pre-conversion warnings;
- conversion summaries;
- sample-rate transition labels; and
- effective output-rate naming.

Regression coverage explicitly checks disclosure for:

- Opus 44.1 -> 48 kHz;
- AC-3 22.05 -> 32 kHz; and
- AAC 192 -> 96 kHz.

The hard-ceiling refusal disclosure remains on the exact direct-rate predicate and is not converted
into ordinary fallback.

### 6. Required CUE regression is preserved

`convert::pipeline::stages::chunk_2_1_3_postprocessing_gate_and_phase_tests::cue_stream_auto_alac_and_lossy_targets_remain_direct_ffmpeg`

is byte-for-byte unchanged from the supplied bundle. A separate regression checks the corrected
Opus command in more detail: 44.1 kHz raw CUE input, one explicit SoXR 44.1 -> 48 kHz filter,
and the final 48 kHz encoder pin.

## Scope / invariants

Exactly six existing source files differ from the supplied bundle:

1. `src/convert/pipeline/stages.rs`
2. `src/convert/pipeline/track_executor.rs`
3. `tonepoet-pipeline/src/mapping.rs`
4. `tonepoet-pipeline/src/plan.rs`
5. `tonepoet-pipeline/src/plugins.rs`
6. `tonepoet-pipeline/src/settings.rs`

No TUI/preset source was changed.

`crates/tonepoet-true-peak` is byte-identical to the supplied baseline.

No process spawning, filesystem I/O or interactive behavior was added to `tonepoet-pipeline`.
No process-global test mutation, key binding, F-key reference, emoji or decorative Unicode was
added by this change.

## Validation performed in this environment

The environment has no `nix`, `cargo`, `rustc` or `rustfmt`, so **no Rust compile/test gate is
claimed**.

The following checks were completed:

- source diff audit: exactly the six files listed above changed;
- `crates/tonepoet-true-peak` subtree hash comparison: identical;
- required existing CUE regression extraction/comparison: identical;
- old downward-only helper reference sweep over Rust sources: zero references;
- added-line whitespace scan: clean;
- lexical Rust delimiter scan over all six modified files: balanced;
- guardrail scan of added lines: no process/I/O imports in the pure planner, no process-global
  test-state mutation, and no F-key additions;
- source patch dry application against fresh copies of all six supplied baseline files: clean, and
  every resulting file hashes identically to this delivery;
- system-FFmpeg syntax/behavior micro-check (not the project acceptance gate): a generated 15 kHz,
  44.1 kHz tone encoded through the exact default explicit SoXR 44.1 -> 48 kHz filter plus
  `libopus` completed successfully, reported a 48 kHz Opus stream, and measured -21.9 dB mean
  energy after a 12 kHz high-pass, confirming the top octave is retained on that exercised path.

## Operator acceptance gate

Run from the repository root inside the project Nix development shell:

```bash
nix develop --extra-experimental-features 'nix-command flakes'
cargo check
cargo test --workspace --no-fail-fast
```

The expected corrective result is that
`cue_stream_auto_alac_and_lossy_targets_remain_direct_ffmpeg` passes without modification.
The two known low-rate flakes named in the brief remain outside this work's responsibility.
