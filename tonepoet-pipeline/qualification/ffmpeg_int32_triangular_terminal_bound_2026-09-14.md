# FFmpeg Int32 triangular terminal bound — x86_64 commissioned, AArch64 pending

Date: 2026-09-14

Authority ID:

`ffmpeg_7-full-n7.1.3+nixpkgs-dd9b079222d43e1943b6ebd802f04fd959dc8e61/libswresample-dbl-triangular-s32/v1`

## Candidate realization

This authority covers exactly:

- backend: FFmpeg;
- FFmpeg release: 7.1.3 from the repository-pinned `nixpkgs` revision
  `dd9b079222d43e1943b6ebd802f04fd959dc8e61`;
- package attribute: `ffmpeg_7-full`;
- candidate runtime architectures: x86_64 and AArch64, the two implementation families
  inspected for architecture-specific libswresample overrides; **x86_64 is commissioned in
  this tree; AArch64 remains closed until the exact pinned executable is exercised there**;
- terminal input: finite Float64 PCM, including the retained Q1.31-derived Binary64 carrier;
- terminal output: signed Int32 PCM packaged as FLAC, WAV, AIFF, or lossless WavPack;
- dither: explicit Tonepoet `Tpdf`, lowered to FFmpeg `dither_method=triangular`;
- dither owner: the selected FFmpeg terminal;
- boundary: lossless stored PCM.

It does **not** cover `SlopedTpdf`, triangular high-pass, any noise-shaped FFmpeg mode,
SoX dither, WavPack hybrid, a different FFmpeg source/build closure, a different
terminal owner, an uninspected runtime architecture, or a terminal whose input carrier
is not Float64. Tonepoet's current lowering similarity between more than one semantic
dither token is deliberately not used as qualification by analogy.

### Tonepoet FFmpeg dither inventory

The current FFmpeg mapping is modeled one semantic cell at a time:

- `Tpdf` -> `triangular`: production-qualified on commissioned x86_64 builds for the
  exact realization above; AArch64 remains closed until its exact-closure commissioning completes;
- `SlopedTpdf` -> `triangular`: not qualified by mapping similarity;
- `Lipshitz` -> `lipshitz`: not qualified;
- `FWeighted` -> `f_weighted`: not qualified;
- `ModifiedEWeighted` -> `modified_e_weighted`: not qualified;
- `ImprovedEWeighted` -> `improved_e_weighted`: not qualified;
- `Shibata` -> `shibata`: not qualified;
- `LowShibata` -> `low_shibata`: not qualified;
- `HighShibata` -> `high_shibata`: not qualified; and
- `Gesemann`: no FFmpeg mapping.

`None` remains the existing undithered terminal authority and is not part of this
qualification.


## Commissioning state

### x86_64 — commissioned 2026-09-16

The strict implementation-conformance harness completed against the exact policy-owned
FFmpeg 7.1.3 executable:

- canonical executable: `/nix/store/5iawqc7p20fpg5k04m9srjkwxv3kyh1m-ffmpeg-full-7.1.3-bin/bin/ffmpeg`;
- executable SHA-256: `8bc4cbb02479983b40efe57c014f8041b62f941635364c02c3adc5bf64cd8d53`;
- version: `7.1.3`;
- identity match: true;
- adversarial samples: 589,947;
- theoretical-bound violations: 0;
- observed maximum error: approximately `1.000000238419` S32 LSB, below the retained
  `2.0000004768371586` S32-LSB deterministic bound.

The reduced mathematical verifier passed. With the x86_64 commissioning gate enabled in a
candidate tree, the focused `tonepoet-pipeline` planner tests passed: certified Track/Album
Guard/Normalize requests admitted the exact TPDF/Int32 FFmpeg cell, forced FFmpeg admitted it,
unqualified dither modes remained refused, and format/WavPack-hybrid boundaries remained
closed. The final workspace root library build is also required to pass after promotion.

Accordingly, `FFMPEG_INT32_TRIANGULAR_X86_64_COMMISSIONED` is `true`. Runtime admission
continues to revalidate the canonical realization, command shape, repository-pinned nixpkgs
identity, policy/build/runner executable path agreement, executable digest binding, cleared
subprocess environment, and exact FFmpeg 7.1.3 version before execution.

### AArch64 — pending

`FFMPEG_INT32_TRIANGULAR_AARCH64_COMMISSIONED` remains `false`. No x86_64 result is
transferred by analogy. AArch64 requires its own strict harness run with exact expected path,
SHA-256, and version, `identity_match = true`, zero theoretical-bound violations, and focused
Rust tests/build passing on that architecture.

## Actual implementation inspected

The exact FFmpeg tag `n7.1.3` was traced through the terminal path:

1. `libswresample/swresample.c` selects planar double (`DBLP`) for the Float64
   swresample path. With equal input/output rates and no forced resampling, the
   terminal does not initialize a rate-conversion stage.
2. `libswresample/dither.c::swri_get_dither` advances the unsigned 32-bit LCG
   `seed = seed * 1664525 + 1013904223`. Plain triangular dither subtracts two
   successive values normalized by `UINT_MAX`. Therefore every possible PRNG state,
   seed and state progression yields a difference in `[-1, 1]`. For floating input
   to S32, FFmpeg's scale is `2^-31` before the configured dither scale; the default
   dither scale is one and Tonepoet does not override it.
3. `libswresample/swresample.c` takes the plain-dither branch for `triangular`; it is
   below the noise-shaping enum range. The generated dither buffer is mixed with the
   signal before final sample-format conversion. No feedback/noise-shaping state is
   involved in this mapped mode.
4. `libswresample/rematrix_template.c` uses double sample/coefficient/intermediate
   types for the DBLP path. The relevant identity mix adds the signal and dither in
   binary64.
5. `libswresample/audioconvert.c` converts DBL to S32 with
   `av_clipl_int32(llrint(sample * 2^31))`: exact binary scaling, C `llrint`, then
   signed-32 saturation.
6. The inspected x86 and AArch64 audio-convert hooks do not replace DBL -> S32, and
   x86 rematrix SIMD has no DBLP replacement for this operation. Runtime admission is
   correspondingly restricted to x86_64 and AArch64. The proof also does not require
   round-to-nearest: it uses a full binary64 ULP and a full target LSB, which cover
   every standard C/IEEE rounding direction. The exact runtime executable is
   additionally path- and SHA-256-bound, so a materially different build cannot
   inherit this authority silently.

The candidate terminal also does not perform channel rematrixing. Tonepoet supplies
raw Float64 input with one input `-ac N` and no output channel-count/layout override.
In FFmpeg n7.1.3 the raw-audio input option is converted into the normal input channel
layout for that count, the `aresample` filter is configured from the negotiated input
and output layouts, and `swr_init` enables rematrixing only when those layouts differ
(or an explicit rematrix control is present). The qualified command shape permits no
such output-layout/rematrix control and requires exactly one `-ac`, so the admitted
libswresample operation is channel-wise signal-plus-dither rather than a channel mix.
This is a proof premise: a future lowering that adds another `-ac`, an explicit channel
layout, or any other audio option is rejected by the runtime command-shape gate rather
than inheriting this bound.

The pinned nixpkgs expression fetches FFmpeg tag `n7.1.3`. Its applicable patch list
was inspected; the patches concern NVCC language flags, hardcoded tables, Chromium
private API compatibility and LCEVC decoder compatibility. None modifies
`libswresample/dither.c`, `swresample.c`, `rematrix_template.c`, or `audioconvert.c`.
FFmpeg n7.1.3 `libswresample/options.c` sets `dither_scale=1`,
`output_sample_bits=0`, and `flags=0` by default. Tonepoet does not override those
values for this terminal. The certified carrier is already at final target rate before
observation and gain, so this authority covers terminal realization rather than a
second rate-change operation.

## Deterministic stored-sample bound

Let one normalized signed-S32 LSB be

`L = 2^-31`.

For every admitted input value and every possible PRNG state:

1. **Dither support.** Each normalized LCG value lies in `[0, 1]`; their difference
   lies in `[-1, 1]`. After the exact power-of-two S32 dither scale,
   `|e_dither| <= L`.
2. **Binary64 dither addition.** In the admitted near-full-scale interval, one full
   binary64 ULP is `2^-52` FS. Charging the full ULP covers the addition regardless of
   the active standard rounding direction: `|e_add| <= 2^-52`.
3. **Integer conversion.** Multiplication by `2^31` is exact binary scaling in the
   admitted finite interval. Before saturation, `llrint` under any standard rounding
   direction differs from its real argument by less than one integer unit. In
   normalized units, `|e_round| < L`; Tonepoet conservatively charges `L`.

Thus the one combined dither-plus-quantization terminal effect is

`E_stored = next_up(L + 2^-52 + L)`.

Numerically, `E_stored = 9.313227966600836e-10` FS, or
`2.0000004768371586` S32 LSB: just over `2 + 2^-21` LSB. It is not an
observed maximum, percentile, safety factor, or textbook TPDF assumption.

## Why saturation is unreachable in the certified cell

The preceding conversion bound is used only on the hard-ceiling route, where the
existing certified observation and gain solver supply the missing range premise.
The certified peak finalizer includes each channel's exact Float64 carrier sample
maximum in the certified interval. Therefore the solver's `P` also bounds the actual
stored carrier samples, not only reconstructed inter-sample peaks.

Let:

- `H = HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER = 4.68`;
- `B_s = 2^-51`, the existing separate Binary64 scalar-multiplication error;
- `B_t = E_stored`;
- `E_post >= H*B_t + H*B_s`, with the existing upward rounding; and
- `C <= 1`, because the admitted hard ceiling is no greater than 0 dBTP.

The existing solver admits a scalar only when

`G * P + E_post <= C`.

An actually multiplied Float64 sample is therefore bounded by

`|x_scaled| <= G*P + B_s <= 1 - E_post + B_s`.

Before `llrint`, worst-case triangular dither and binary64 addition rounding can add
at most `L + 2^-52`, giving

`|x_pre_llrint| <= 1 - E_post + B_s + L + 2^-52`.

With the candidate constants this is `0.9999999961070691`, while the positive S32
no-saturation threshold for an arbitrary standard rounding direction is
`1 - L = 0.9999999995343387`; the negative value remains above `-1`. Saturation is
therefore unreachable for every value admitted by this certified solver. This is why
`av_clipl_int32` contributes no additional unbounded clipping term.

`tonepoet-pipeline/qualification/verify_ffmpeg_int32_triangular_terminal_bound.py`
checks the extremal constituent states, exact neighboring binary64 values around
half-LSB transitions, all four standard rounding directions, full-scale vicinity,
and this solver-derived no-saturation inequality.

## Placement in Tonepoet's terminal calculus

The stored bound is reconstructed using the existing single authority:

`E_terminal_reconstructed = next_up(E_stored * HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER)`.

The existing in-process Float64 scalar multiplication allowance remains separate
because it is a different physical operation before FFmpeg. The old undithered
FFmpeg half-LSB terminal term is **replaced** by `E_stored` for this exact
realization; it is not charged beside it. Consequently there is one charge for the
one physical dither-plus-quantization terminal effect.

The participant inequality remains unchanged:

`G * (P_signal + E_pre) + E_post <= C`.

No extra audio traversal, probabilistic argument, runtime dither estimator, or second
gain solver is introduced.

## Implementation identity

Runtime support reuses the existing packaged-tool identity plumbing. Admission of
this authority requires:

- the exact canonical terminal realization above;
- the lowered command must remain raw `f64le` ingress at the already-selected rate,
  exactly one input `-ac`, no output channel-count/layout override, one simple
  `aresample` stage with `dither_method=triangular` and `out_sample_fmt=s32`, and the
  expected lossless encoder; unrecognized audio-filter/aresample options,
  `SWR_FLAG_RESAMPLE`, dither-scale changes, alternate ingress, channel-rematrix
  controls, input options moved into output scope, and late sample-format overrides
  are refused;
- an x86_64 or AArch64 runtime architecture;
- the repository's embedded nixpkgs revision and nar hash;
- the policy-owned packaged FFmpeg path and the build-embedded FFmpeg path to agree;
- the runner-resolved path to equal that canonical path;
- SHA-256 of that exact executable to be captured before execution;
- a cleared subprocess environment with no command-specific environment additions, so ambient loader/interposition variables cannot replace the inspected libswresample/libc path while retaining the same FFmpeg executable;
- exact reported FFmpeg version `7.1.3`; and
- both ordinary command execution and the retained-PCM scalar-pump execution to run
  the already-bound path and reject subsequent path/content drift.

This binds the proof to the actual executable without creating a second Reference
release-qualification system. It does not promote or otherwise alter Reference State
A evidence.

## Empirical conformance role

`scripts/validate_ffmpeg_int32_triangular_terminal_bound.py` generates adversarial
Float64 values at and immediately around S32 quantization boundaries, at positive and
negative full-scale vicinity, and long repeated runs that exercise dither PRNG state
progression. It checks every emitted S32 sample against the theoretical bound.

The script never turns a matching version string into qualification. A qualifying
run must supply the exact expected real path and executable SHA-256 and must match
version 7.1.3. `--allow-unqualified-identity` exists only for explicitly non-qualifying
implementation-model checks on another executable. Empirical results can expose a
mistaken implementation model; they do not define or tighten the bound.
