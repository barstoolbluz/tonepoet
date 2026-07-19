# SoX-ng 14.8.0.1 DSD Decimation Test Report — Revision 5

## Scope

This report tests the exact upstream source archive supplied for:

- Tag: `sox_ng-14.8.0.1`
- Commit identified by the implementation agent: `266aa9e777829a1b60959a1250b4c42597558639`
- Archive SHA-256: `7698a1b2699499b0b38fa95a15bb56c68928d97b144bce03b7ecb76fe9c46698`

The two DSD reader blobs match the expected Git object IDs exactly:

```text
src/dsf.c     b27ca96a3c04d65966c5ff64dc5db8613a2f886a
src/dsdiff.c  8b2b064d80eaf33c4372e9e65bacbe8ca60c0c4e
```

## Build result

The supplied release compiled successfully with GCC 14.2.0.

```text
sox_ng: SoX_ng v14.8.0.1
```

The build included the native DSF and DSDIFF handlers, `rate`, `sinc`, `sdm`, and the normal effects chain.

The ordinary source-level format conversion tests completed successfully. The separate regression script reported all runnable regression/security tests as `OK`; tests requiring unavailable optional codecs were `VOID`. The initial out-of-tree `make check` invocation failed because the test Makefile attempted to compile `samples.c` from the build directory rather than the source directory. Compiling that helper from the supplied source and rerunning the affected `bit-depth` test produced `OK`.

## Native DSD format limits

In this exact release, the native DSF and DSDIFF handlers advertise and accept these rates:

```text
DSD64   2.8224 MHz
DSD128  5.6448 MHz
DSD256  11.2896 MHz
```

They do not advertise DSD512 or DSD1024. Tonepoet therefore cannot use this native SoX-ng DSF/DFF path for those rates without extending the handlers. DSD512+ requires another decoder/backend, such as a qualified FFmpeg path, or a future SoX-ng implementation.

## Confirmed reader behavior

Both readers expose one sample per DSD bit at the native DSD rate and map the bit to the signed 32-bit effects-chain endpoints:

```text
1 → +SOX_SAMPLE_MAX
0 → -SOX_SAMPLE_MAX
```

They apply no reconstruction filter, no sample-rate conversion, no normalization, and no fixed `+6.0206 dB` compensation.

A deterministic 50%-modulation bit pattern decoded to exactly:

```text
0.5 linear = -6.0205999 dBFS
```

An alternating positive/negative pattern decoded to exact zero after decimation.

The same one-bit sequence stored as DSF and DSDIFF produced sample-identical decoded PCM:

```text
maximum difference: 0
RMS difference:     0
```

## Candidate effect orders

The bake-off compared:

```text
A. gain/headroom → sinc → rate -u → gain restoration
B. gain/headroom → rate -u → sinc → gain restoration
C. gain/headroom → rate -u → gain restoration
```

All output used 64-bit floating-point WAV containers so the final file encoding did not add integer quantization or dither. SoX-ng's effects-chain boundaries remained its normal signed 32-bit representation.

## Steady-state frequency-response result

With identical explicit headroom, `sinc → rate` and `rate → sinc` produced essentially the same steady-state response.

For DSD64 converted to 88.2 kHz with a nominal 25–35 kHz profile:

```text
1 kHz:   unity relative response
25 kHz:  unity relative response
30 kHz:  approximately -6.02 dB
35 kHz:  pre-order  approximately -142.7 dB
         post-order approximately -140.2 dB
40 kHz:  pre-order  approximately -146.0 dB
         post-order approximately -150.7 dB
```

The earlier revisions placed four order-null values in one table. That presentation was invalid: the rows came from different fixtures, reconstruction profiles, and exploratory settings. In particular, it mixed a former DSD128 48–70 kHz candidate and a DSD256 88.2–140 kHz wideband profile with the Reference profiles. Absolute null level is also signal-dependent because the two orders quantize different intermediate signals at SoX-ng's signed 32-bit effects boundaries.

A controlled rerun used the same method for all current Reference profiles:

```text
source fixture:      3-second sdm-4 DSF, 1 kHz at -6 dBFS modulation
processing reserve:  -12 dB
sinc attenuation:    -a 180
steady-state trim:   0.5 seconds from each end
comparison:          sinc → rate -u versus rate -u → sinc
```

| Source and current Reference profile | Absolute order null | Null relative to output |
|---|---:|---:|
| DSD64 → 88.2 kHz, 25–35 kHz | -152.48 dBFS RMS | -137.45 dB |
| DSD128 → 176.4 kHz, 30–45 kHz | -167.85 dBFS RMS | -152.82 dB |
| DSD256 → 176.4 kHz, 48–70 kHz | -177.22 dBFS RMS | -162.19 dB |

A second controlled `sdm-4` zero-input fixture produced:

| Source and current Reference profile | Absolute order null |
|---|---:|
| DSD64 → 88.2 kHz, 25–35 kHz | -153.61 dBFS RMS |
| DSD128 → 176.4 kHz, 30–45 kHz | -173.44 dBFS RMS |
| DSD256 → 176.4 kHz, 48–70 kHz | -179.90 dBFS RMS |

These are fixture-specific numerical diagnostics, not quality rankings by DSD rate. They establish that the two orders agree far below a 24-bit deliverable threshold for the tested material. They must not be used as stopband-rejection measurements.


## Composite stopband-rejection experiment

The approximately −180 dB results reported by some earlier tests were order-null floors. They measured the numerical difference between:

```text
sinc → rate -u
```

and:

```text
rate -u → sinc
```

They did not measure stopband attenuation.

A separate experiment therefore measured coherent signal rejection directly.

### Method

Two complementary paths were used.

#### Linear transfer-path test

A high-rate PCM sine was generated at the native DSD sample rate and passed through the same SoX-ng effects chain:

```text
−12 dBFS peak sine
  → rate -u
  → profile sinc
  → 64-bit float WAV for measurement
```

This isolates the linear transfer function while retaining SoX-ng's signed 32-bit effects boundaries.

#### End-to-end DSF test

A sine was converted to actual one-bit DSF using SoX-ng's `sdm-4` modulator, then decoded through:

```text
DSF reader
  → explicit −12 dB processing headroom
  → rate -u
  → profile sinc
  → 64-bit float WAV
```

Coherent tone amplitude at the stopband edge was compared with a 1 kHz reference generated at the same modulation level.

### DSD64 result

Profile:

```text
passband through approximately 25 kHz
transition approximately 25–35 kHz
stopband from approximately 35 kHz
target PCM rate 88.2 kHz
```

Actual DSF end-to-end rejection at 35 kHz:

| Requested `sinc` attenuation | Measured coherent rejection |
|---:|---:|
| 140 dB | 140.31 dB |
| 160 dB | 162.08 dB |
| 180 dB | 180.12 dB |

A longer eight-second DSF test measured approximately:

```text
35 kHz:
  184.90 dB relative to the 1 kHz reference

40 kHz:
  187.14 dB relative to the 1 kHz reference
```

The longer results benefit from greater coherent averaging and should not be interpreted as exact filter precision.

A direct linear-path sweep with `sinc -a 180` covered 35–44 kHz in 1 kHz steps and four sine phases. The weakest measured rejection was approximately:

```text
183.59 dB
```

The output was at the signed 32-bit effects-chain quantization floor.

### DSD128 result

Profile:

```text
passband through approximately 30 kHz
transition approximately 30–45 kHz
stopband from approximately 45 kHz
target PCM rate 176.4 kHz
```

An actual DSF end-to-end test with `sinc -a 180` measured:

```text
coherent rejection at 45 kHz:
  approximately 189.03 dB
```

### DSD256 result

Profile:

```text
passband through approximately 48 kHz
transition approximately 48–70 kHz
stopband from approximately 70 kHz
target PCM rate 176.4 kHz
```

An actual DSF end-to-end test with `sinc -a 180` measured:

```text
coherent rejection at 70 kHz:
  approximately 201.23 dB
```

This value is beyond the nominal signed 32-bit sample-step range and should be read as a noise-floor-limited lower bound, not as literal 201-bit-style arithmetic precision.

### FIR cost

Increasing the requested reconstruction attenuation from 140 to 180 dB increased the PCM-rate `sinc` lengths as follows:

| Profile | 140 dB | 160 dB | 180 dB |
|---|---:|---:|---:|
| DSD64, 25–35 kHz at 88.2 kHz | 84 taps | 96 taps | 109 taps |
| DSD128, 30–45 kHz at 176.4 kHz | 111 taps | 128 taps | 145 taps |
| DSD256, 48–70 kHz at 176.4 kHz | 76 taps | 88 taps | 99 taps |

Because the explicit `sinc` runs after decimation at the target PCM rate, the incremental cost is modest relative to the native-rate `rate -u` stage.

### Interpretation

The pinned build can achieve approximately 180 dB coherent stopband rejection for all three native DSD rates when the profile `sinc` requests 180 dB attenuation.

The correct product wording is:

```text
Reference filter setting:
  180 dB requested attenuation

Reference qualification target:
  approximately 180 dB coherent composite stopband rejection
```

It is incorrect to infer:

```text
full-band PCM noise floor:
  −180 dBFS
```

The full-band output may contain substantially more in-passband DSD-shaped noise. Stopband rejection and output dynamic range are separate measurements.

## Controlled DSD128 passband and transition experiment

A separate sweep tested five deterministic SoX-ng DSD128 modulators:

```text
sdm-4
sdm-5
sdm-6
sdm-7
sdm-8
```

Each fixture used:

```text
explicit -12 dB headroom
  → rate -u
  → linear-phase sinc with 140 dB requested attenuation
  → 64-bit float WAV for measurement
```

The first and last 0.5 seconds were excluded from steady-state measurements. Results were collected at both 176.4 and 192 kHz.

The same reconstruction profile produced effectively identical steady-state noise at 176.4 and 192 kHz. Once both targets could contain the profile, retained bandwidth—not PCM rate—determined the result.

### DSD128 profile sweep

Full-band output noise after restoring the fixed 12 dB measurement attenuation:

| Transition | Sinc taps at 176.4 kHz | Median full-band noise | Worst tested |
|---|---:|---:|---:|
| 25–35 kHz | 166 | -151.58 dBFS | -129.73 dBFS |
| 28–42 kHz | 119 | -139.74 dBFS | -121.80 dBFS |
| **30–45 kHz** | **111** | **-134.82 dBFS** | **-118.47 dBFS** |
| 35–50 kHz | 111 | -126.23 dBFS | -112.56 dBFS |
| 40–60 kHz | 84 | -115.82 dBFS | -105.41 dBFS |
| 44–65 kHz | 80 | -109.16 dBFS | -101.68 dBFS |
| 48–70 kHz | 76 | -104.60 dBFS | -98.30 dBFS |
| 56–80 kHz | 70 | -96.07 dBFS | -91.67 dBFS |

Noise below 20 kHz remained effectively unchanged. The difference was additional ultrasonic DSD-shaped noise admitted by wider profiles.

The widening penalty was continuous:

```text
30–45 kHz → 35–50 kHz:
  approximately 8.6 dB more median full-band noise

35–50 kHz → 40–60 kHz:
  approximately 10.4 dB more

40–60 kHz → 48–70 kHz:
  approximately 11.2 dB more
```

The measured practical knee is therefore approximately:

```text
passband through 30 kHz
transition 30–45 kHz
stopband from approximately 45 kHz
```

### DSD128 product interpretation

Recommended profiles:

```text
Conservative:
  25–35 kHz

Auto / Reference:
  30–45 kHz

Wideband Preservation:
  35–50 kHz

Aggressive expert:
  48–70 kHz
```

The earlier 48–70 kHz DSD128 proposal is not a strong Auto default. It retained approximately 30 dB more median full-band ultrasonic noise than 30–45 kHz.

### DSD128 filter-order check

The original DSD128 null figures were not retained with enough fixture metadata to support a reproducible general claim. A controlled rerun with `sdm-4`, `sinc -a 180`, identical -12 dB headroom, and 0.5-second steady-state trimming measured:

```text
30–45 kHz Reference, zero-input DSF:
  -173.44 dBFS RMS

30–45 kHz Reference, 1 kHz DSF:
  -167.85 dBFS RMS

35–50 kHz Wideband, zero-input DSF:
  -168.98 dBFS RMS
```

The values vary with fixture spectrum and level; they are not filter-rejection specifications. The result still supports the established implementation:

```text
rate -u → profile sinc
```

The PCM-rate `sinc` implements the same steady-state response with a short FIR and lower computational cost.

### DSD128 limitations

The sweep used synthetic SoX-ng modulators. Commercial recordings may contain additional ADC noise, analog-front-end noise, intentional ultrasonic signal, interference, or artifacts from previous conversion.

The result nevertheless establishes the relative cost of widening the profile and demonstrates that 48–70 kHz is not noise-neutral.

## Controlled DSD256 target-rate and bandwidth experiment

The original comparison paired two changes:

```text
176.4 kHz output with a 48–70 kHz transition
352.8 kHz output with an 88.2–140 kHz transition
```

That comparison could not distinguish the effect of doubling the PCM rate from the effect of widening the retained DSD bandwidth.

A second controlled experiment held the source at DSD256 and the output rate at 352.8 kHz while changing only the reconstruction profile.

### Order-null result

A controlled current rerun used the same `sdm-4` zero-input DSF, -12 dB headroom, `sinc -a 180`, and 0.5-second steady-state trimming:

```text
DSD256 → 176.4 kHz, transition 48–70 kHz:
  -179.90 dBFS RMS

DSD256 → 352.8 kHz, transition 48–70 kHz:
  -180.45 dBFS RMS

DSD256 → 352.8 kHz, transition 88.2–140 kHz:
  -147.33 dBFS RMS
```

Holding the 48–70 kHz profile constant therefore produced essentially the same null floor at 176.4 and 352.8 kHz. The much weaker wideband null reflects the larger ultrasonic signal presented to the finite-precision inter-effect boundary; it is not evidence that the wideband filter has poorer stopband rejection.

### Full-band output noise

Using the deterministic DSD256 modulator-noise fixture:

```text
176.4 kHz / 48–70 kHz:
  -153.66 dBFS RMS

352.8 kHz / 48–70 kHz:
  -153.67 dBFS RMS

352.8 kHz / 70–110 kHz:
  -120.91 dBFS RMS

352.8 kHz / 88.2–140 kHz:
  -102.99 dBFS RMS
```

The 176.4 and 352.8 kHz outputs had effectively identical full-band noise when both used the 48–70 kHz profile.

The wider profiles retained progressively more ultrasonic DSD modulator noise. The full-band RMS figure therefore became much worse even though the lower-frequency result did not.

### Band-limited noise

For all three tested 352.8 kHz profiles, noise below 48 kHz remained essentially unchanged:

```text
0–20 kHz:
  approximately -191.7 to -191.8 dBFS RMS

20–48 kHz:
  approximately -169.2 dBFS RMS
```

The divergence occurred above 48 kHz, in the additional ultrasonic region admitted by the wider transitions.

### Direct 176.4-versus-352.8 comparison

With the same 48–70 kHz profile, the 352.8 kHz output was sufficiently suppressed above 70 kHz that every second sample could be compared directly with the 176.4 kHz output.

```text
1 kHz fixture:
  null after 2:1 alignment: -180.39 dBFS RMS
  level difference:        effectively zero

DSD256 modulator-noise fixture:
  null after 2:1 alignment: -180.83 dBFS RMS
```

The two outputs are, for practical purposes, the same 48–70 kHz DSD256 conversion represented at different PCM sample rates.

### Interpretation

The previously weaker 352.8 kHz result was caused by the wider 88.2–140 kHz reconstruction profile, not by the 352.8 kHz target rate.

The target sample rate determines what bandwidth can be represented. It should not automatically determine how much bandwidth Auto preserves.

## Boundary behavior

The two orders differ at the beginning and end of a finite file because their filters use different boundary handling and run at different stages.

Across the tested DSD64 tones, `rate -u → sinc` produced boundary residuals that were equal to or lower than `sinc → rate`. The improvement was modest in the passband and larger near the upper retained/transition region.

No tested case showed a finite-file boundary advantage for placing `sinc` before `rate -u`.

## Computational cost

The post-decimation `sinc` is dramatically shorter because it is designed at the target PCM rate rather than the native DSD rate.

### DSD64 → 88.2 kHz, 25–35 kHz profile

```text
sinc before rate: 2630 reported taps
sinc after rate:    84 reported taps
```

Ten-second conversion benchmark:

```text
sinc → rate: 1.05 s elapsed
rate → sinc: 0.68 s elapsed
```

The post-decimation order was approximately 35% faster.

### DSD256 → 176.4 kHz, 48–70 kHz profile

```text
sinc before rate: 4782 reported taps
sinc after rate:    76 reported taps
```

Five-second conversion benchmark:

```text
sinc → rate: 3.01 s elapsed
rate → sinc: 2.09 s elapsed
```

The post-decimation order was approximately 31% faster.

The resampler remains a substantial part of total cost, so the wall-clock speedup is smaller than the tap-count ratio, but it is repeatable and material.

## Aliasing conclusion

`rate -u` is not a sample-drop operation. It is a complete high-rejection band-limited resampler. In this release its help reports:

```text
95% bandwidth
200 dB nominal rejection
linear phase by default
```

Therefore it is safe, in principle and in these tests, to let `rate -u` perform the DSD-rate-to-PCM-rate antialias decimation before applying a narrower product-bandwidth `sinc` at the target PCM rate.

The later `sinc` does not need to undo aliasing. `rate -u` has already prevented it. The later `sinc` exists to narrow the retained PCM bandwidth from the resampler's near-Nyquist response to Tonepoet's selected source/target profile.

## Headroom result

SoX-ng's global `-G` guard is not sufficient when an explicit `sinc` is present.

Source inspection and verbose effect-chain traces show why:

- `sinc` is flagged as a gain-class effect.
- With `sinc` first, `-G` does not insert headroom before it.
- With `rate` first, `-G` inserts headroom before `rate`, then reclaims it before `sinc`.
- The `sinc` stage can therefore remain exposed in either order.

Stress-test results included:

```text
100% positive DSD pattern, -G, sinc → rate:
  sinc clipped 1320 samples

100% positive DSD pattern, -G, rate → sinc:
  sinc clipped 36 samples

abrupt negative-to-positive DSD step, -G, sinc → rate:
  sinc clipped 2644 samples
```

With explicit `-12 dB` attenuation before the complete DSP chain, none of the tested patterns clipped in either order.

The evaluated profile FIRs had absolute-coefficient sums corresponding to approximately 5.5–6.3 dB worst-case FIR headroom, while `rate` itself reserves approximately 3 dB through its internal gain estimate. A provisional 12 dB processing reserve is therefore conservative for this chain and still leaves substantially more than 24 bits of effective resolution inside the signed 32-bit effects representation.

Tonepoet should qualify the minimum safe reserve across every supported profile, but it should not rely on `-G`.

## Evidence-based implementation recommendation

For SoX-ng 14.8.0.1, use this Auto/Reference structure for native DSD64, DSD128, and DSD256:

```text
native DSF/DFF reader
  → explicit fixed processing-headroom attenuation
  → rate -u directly to the requested PCM sample rate
  → profile-specific linear-phase sinc at the target PCM rate
  → PCM-domain effects
  → peak/true-peak analysis
  → one constant gain restoration and DSD level compensation
  → one final integer quantization
  → TPDF at 24 bit or qualified Shibata-family shaping at 16 bit
  → lossless encoding/container output
```

For 44.1 and 48 kHz output, use the qualified `rate -u` response as the integrated reconstruction/decimation filter. A separate ultrasonic `sinc` is generally unnecessary because the destination Nyquist limit already defines the narrow transition.

For 88.2 kHz and higher, apply the narrower Tonepoet profile after decimation:

```text
rate -u → sinc
```

This order is recommended because it:

- provides the same steady-state response as pre-decimation `sinc` to below 24-bit relevance;
- showed equal or better finite-file boundary behavior;
- requires far fewer FIR taps;
- was approximately 31–35% faster in the long fixtures;
- avoids filtering the full-rate DSD stream with a very long explicit FIR;
- leaves `rate -u` responsible for the job it is already designed to perform: antialias decimation.

## Product bandwidth rule

The DSD128 and DSD256 experiments require a refinement of the earlier two-ceiling rule.

Sample rate and reconstruction bandwidth should be separate controls. Source and target rates constrain the widest permissible profile, but Auto need not select that widest profile.

```text
effective reconstruction profile =
  narrower of:
    requested or Auto-default profile
    qualified source DSD ceiling
    target PCM ceiling
```

A lower target rate can narrow the selected profile. A higher target rate never widens a lower-rate DSD source beyond its source ceiling, and it does not automatically widen a higher-rate source to the broadest representable profile.

Examples:

```text
DSD64 → 176.4 kHz:
  retain the 25–35 kHz DSD64 profile

DSD128 → 176.4 or 192 kHz, Auto:
  use the 30–45 kHz Reference profile

DSD128 → 176.4 or 192 kHz, explicit Wideband Preservation:
  use the 35–50 kHz profile

DSD256 → 176.4 kHz, Auto:
  use the 48–70 kHz Reference profile

DSD256 → 352.8 kHz, Auto:
  also use the 48–70 kHz Reference profile

DSD256 → 352.8 kHz, explicit Wideband Preservation:
  use the provisional 88.2–140 kHz profile
  and disclose the substantially greater retained ultrasonic noise
```

The measured results support:

```text
DSD128 default:
  30–45 kHz

DSD256 default:
  48–70 kHz
```

A user may select a higher PCM rate without being forced into a wider reconstruction profile.

## Final verdict

Yes: this SoX-ng path can perform textbook-quality DSD decimation.

For this exact release, the empirically supported implementation is:

```text
explicit headroom → rate -u → profile sinc at -a 180 → effects → measured gain → final quantization
```

—not a mandatory pre-decimation `sinc`, and not reliance on `-G`.

The additional DSD256 experiment establishes a second implementation rule:

> Do not couple reconstruction bandwidth automatically to the selected PCM sample rate.

For DSD128, the five-modulator sweep found a practical Auto knee at 30–45 kHz. The earlier 48–70 kHz proposal retained approximately 30 dB more median full-band ultrasonic noise without changing noise below 20 kHz.

At equal 48–70 kHz bandwidth, DSD256 → 176.4 kHz and DSD256 → 352.8 kHz were effectively identical after 2:1 alignment. The much worse full-band noise of the earlier 352.8 kHz result came from retaining the 88.2–140 kHz ultrasonic region, not from the higher PCM rate itself.


## Stopband-rejection recommendation

For the pinned native SoX-ng path, use:

```text
sinc -a 180
```

and qualify the complete path for approximately 180 dB coherent stopband rejection.

Keep order-null results, full-band noise measurements, and stopband-rejection measurements separate in all reports and acceptance criteria.

Revision 5 removes the mixed exploratory order-null table from earlier revisions and replaces it with reproducible current-profile tests.
