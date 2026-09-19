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
