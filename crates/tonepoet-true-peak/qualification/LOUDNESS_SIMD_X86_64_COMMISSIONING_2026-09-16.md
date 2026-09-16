# NativeEbu2023 loudness SIMD x86_64 commissioning — 2026-09-16

## Scope

This record commissions only the x86_64 SSE2 backend in `src/loudness/simd.rs` for production `NativeEbu2023` loudness arithmetic. It does not commission AVX, change loudness arithmetic, or alter any true-peak SIMD qualification.

## Correctness gate

Release-mode differential qualification passed 9/9 targeted SIMD tests on the exact source state before dispatch promotion. Coverage includes K-weighting output/state bit identity, rounding-sensitive window sums, full/reduced meter state, invalid-block atomicity, DAZ/FTZ behavior, reset behavior, and bounded history/storage behavior.

Command:

```text
cargo test --offline -p tonepoet-true-peak --lib --release simd -- --nocapture
```

Result: 9 passed, 0 failed, 115 filtered out.

## Performance gate

The acceptance protocol was declared before inspecting formal results. Mono and stereo used 600 programme-seconds per measured run. Six- and eight-channel supporting fixtures were reduced to 120 programme-seconds before any measured result for those channel counts was inspected because the original 600-second supporting runs exceeded the execution window. Each fixture used two warmups and nine measured repetitions with rotating backend order.

Promotion criteria were:

- stereo median at least 5% faster than scalar and faster in at least 7/9 paired rounds;
- mono, six-channel, and eight-channel median no more than 2% slower than scalar;
- exact result-bit identity;
- if multiple candidates pass and stereo medians differ by less than 2%, prefer the narrower ISA requirement.

| Channels | Programme seconds | Scalar median | SSE2 median | SSE2 vs scalar | Paired wins | AVX median | AVX vs scalar | AVX paired wins |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 600 | 1,418,264,661 ns | 1,353,917,665 ns | +4.537% | 6/9 | 1,302,236,175 ns | +8.181% | 6/9 |
| 2 | 600 | 2,417,521,666 ns | 1,607,144,166 ns | +33.521% | 9/9 | 1,617,831,066 ns | +33.079% | 9/9 |
| 6 | 120 | 2,027,988,438 ns | 1,372,311,070 ns | +32.331% | 8/9 | 1,031,659,611 ns | +49.129% | 9/9 |
| 8 | 120 | 4,161,523,705 ns | 2,474,048,401 ns | +40.549% | 9/9 | 1,998,979,144 ns | +51.965% | 9/9 |

Each channel-count matrix produced one unique result-bit tuple across scalar, SSE2, and AVX.

SSE2 and AVX both pass the declared promotion gate. Their stereo medians differ by less than 2%, so the predeclared rule selects SSE2 as the narrower ISA requirement. AVX remains non-production; its larger multichannel gains are evidence for a future independently justified promotion, not authority to widen this one.

## Build / host identity

- Rust: 1.93.1
- target: x86_64 Linux
- production workspace vectorizer workaround retained: `-C llvm-args=--vectorize-loops=false`
- CPU used for commissioning: Intel Xeon Platinum 8573C
- runtime feature detection remains the authority for selecting SSE2; scalar remains the fallback when SSE2 is unavailable on a target build.

## Promotion boundary

For `NativeEbu2023`, production dispatch changes only from scalar to SSE2 on x86_64 hosts reporting SSE2. `Libebur128126` remains scalar because it is outside this commissioning scope. The scalar implementation remains the native fallback when SSE2 is unavailable. AVX remains test-only. Numerical contracts, public API, meter state, and result representation are unchanged.
