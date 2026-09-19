# NativeEbu2023 loudness SIMD x86_64 re-attestation on operator hardware — 2026-09-19

## Scope

Re-runs the performance gate of `LOUDNESS_SIMD_X86_64_COMMISSIONING_2026-09-16.md` on the
operator's machine, using the same predeclared criteria. No dispatch change is made by
this record; it reports evidence.

## Host

- CPU: Intel Xeon E5-1680 v2 (Ivy Bridge-EP), 16 threads, AVX without AVX2/FMA
- Rust 1.93.1, x86_64 Linux, workspace vectorizer workaround retained
- Harness: `examples/loudness_simd_timing.rs` driven by
  `qualification/loudness_simd_protocol.py` (two warmups, nine measured rounds,
  rotating backend order, deterministic synthetic programme at 48 kHz)
- Raw samples: `qualification/loudness_simd_x86_64_ivybridge_2026-09-19.json`

## Correctness

`cargo test -p tonepoet-true-peak --lib --release simd -- --test-threads=1`: 9 passed.

## Performance

| Channels | Programme seconds | Scalar median | SSE2 median | SSE2 vs scalar | Paired wins | AVX median | AVX vs scalar | AVX paired wins | Bit-identical |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| 1 | 600 | 1,145,599,195 ns | 1,196,389,195 ns | -4.433% | 0/9 | 1,223,269,544 ns | -6.780% | 0/9 | yes |
| 2 | 600 | 1,934,771,919 ns | 1,517,872,653 ns | +21.548% | 9/9 | 1,755,959,669 ns | +9.242% | 9/9 | yes |
| 6 | 120 | 1,341,690,398 ns | 923,665,787 ns | +31.157% | 9/9 | 855,088,592 ns | +36.268% | 9/9 | yes |
| 8 | 120 | 2,287,328,330 ns | 1,412,596,929 ns | +38.242% | 9/9 | 1,238,540,030 ns | +45.852% | 9/9 | yes |

A second independent mono run reproduced the deficit: SSE2 -4.392% (0/9), AVX -6.315% (0/9).

Every fixture produced one result-bit tuple across all three backends.

## Verdict against the predeclared criteria

- Stereo: SSE2 and AVX both pass (at least 5% faster, at least 7/9 wins). SSE2 is faster
  than AVX by more than 2% on this host, so the tie rule does not arise; SSE2 wins outright.
- Six and eight channels: both pass.
- Mono: both FAIL the "no more than 2% slower than scalar" criterion, consistently.
- Bit identity: pass.

So the existing SSE2 production dispatch is NOT re-attested on this host as commissioned:
it regresses single-channel programmes by about 4.4%. AVX is not promotable here on any
reading: it fails mono and is slower than SSE2 on stereo.

Open decision: whether production dispatch should fall back to scalar for one-lane meters
(the commissioning host also showed mono as the weakest case, +4.5%). That is a dispatch
change and is not made here.

## Dispatch change and rerun — same day

`Backend::production(channels)` now returns scalar for one-lane meters and SSE2 for two
or more channels (x86_64 with SSE2). The harness gained a `production` mode that leaves
the meter's own dispatch untouched. Full protocol rerun (raw samples:
`loudness_simd_x86_64_ivybridge_mono_scalar_2026-09-19.json`):

| Channels | Programme seconds | Scalar median | SSE2 median | SSE2 vs scalar | Paired wins | AVX median | AVX vs scalar | AVX paired wins | Production median | Production vs scalar | Production wins | Bit-identical |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| 1 | 600 | 1,145,096,647 ns | 1,195,110,098 ns | -4.368% | 0/9 | 1,215,714,171 ns | -6.167% | 0/9 | 1,144,840,625 ns | +0.022% | 7/9 | yes |
| 2 | 600 | 1,932,710,584 ns | 1,518,859,109 ns | +21.413% | 9/9 | 1,754,424,824 ns | +9.225% | 9/9 | 1,518,607,151 ns | +21.426% | 9/9 | yes |
| 6 | 120 | 1,337,383,945 ns | 921,862,845 ns | +31.070% | 9/9 | 858,370,282 ns | +35.817% | 9/9 | 915,672,665 ns | +31.533% | 9/9 | yes |
| 8 | 120 | 2,293,675,789 ns | 1,421,536,694 ns | +38.024% | 9/9 | 1,238,561,616 ns | +46.001% | 9/9 | 1,415,274,213 ns | +38.297% | 9/9 | yes |

Production dispatch now passes every predeclared criterion on this host: mono is within
noise of scalar (it is scalar), stereo clears the 5% / 7-of-9 bar, and no count is slower.

## Quad and five-channel layouts — added

The commissioning protocol never measured 4 or 5 channels. Added with the crate's own
default role layouts (L R Ls Rs; L R C Ls Rs), 120 programme-seconds. Raw samples:
`loudness_simd_x86_64_ivybridge_quad_5ch_2026-09-19.json`.

| Channels | Programme seconds | Scalar median | SSE2 median | SSE2 vs scalar | Paired wins | AVX median | AVX vs scalar | AVX paired wins | Production median | Production vs scalar | Production wins | Bit-identical |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| 4 | 120 | 878,982,001 ns | 552,312,743 ns | +37.164% | 9/9 | 535,801,846 ns | +39.043% | 9/9 | 551,476,901 ns | +37.260% | 9/9 | yes |
| 5 | 120 | 1,215,241,791 ns | 829,753,898 ns | +31.721% | 9/9 | 787,633,720 ns | +35.187% | 9/9 | 836,706,185 ns | +31.149% | 9/9 | yes |

AVX relative to SSE2 (median of medians) on this host: 4 ch +3.0%, 5 ch +5.1%,
6 ch +6.9%, 8 ch +12.9%; stereo -15.5%. AVX remains test-only pending a separate
promotion decision; if promoted, it would be per channel count, never for stereo or mono.

## AVX promoted for four or more channels — same day

Operator decision: AVX becomes production dispatch for four or more channels, falling back
to SSE2 and then scalar when the host lacks the ISA. Stereo and three-channel stay SSE2
(AVX measured 15.5% slower than SSE2 on stereo here and slower on the commissioning host
too). Mono stays scalar. Quad clears the record's own 2% tie rule at +3.0% over SSE2, and
the margin grows with channel count; the commissioning host (Xeon Platinum 8573C) showed
larger AVX margins on six and eight channels than this first-generation AVX part, so the
gain is expected to be larger on newer hardware, not smaller.

Verification of the new dispatch, `production` mode, raw samples in
`loudness_simd_x86_64_ivybridge_avx_dispatch_2026-09-19.json`:

| Channels | Programme seconds | Scalar median | SSE2 median | SSE2 vs scalar | Paired wins | AVX median | AVX vs scalar | AVX paired wins | Production median | Production vs scalar | Production wins | Bit-identical |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :---: |
| 4 | 120 | 882,833,735 ns | 547,625,289 ns | +37.970% | 9/9 | 535,705,205 ns | +39.320% | 9/9 | 536,154,809 ns | +39.269% | 9/9 | yes |
| 5 | 120 | 1,213,189,633 ns | 829,718,260 ns | +31.609% | 9/9 | 787,977,355 ns | +35.049% | 9/9 | 788,415,135 ns | +35.013% | 9/9 | yes |
| 6 | 120 | 1,337,040,526 ns | 913,445,544 ns | +31.682% | 9/9 | 857,412,072 ns | +35.872% | 9/9 | 857,067,646 ns | +35.898% | 9/9 | yes |
| 8 | 120 | 2,290,924,215 ns | 1,413,953,433 ns | +38.280% | 9/9 | 1,243,255,134 ns | +45.731% | 9/9 | 1,241,522,204 ns | +45.807% | 9/9 | yes |

Final production dispatch for `NativeEbu2023` on x86_64: 1 channel scalar; 2 to 3
channels SSE2; 4 or more AVX. `Libebur128126` stays scalar. The selection test
`simd01_runtime_selection_respects_profile_commissioning_boundary` encodes this.
