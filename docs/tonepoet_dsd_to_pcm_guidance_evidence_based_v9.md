# Tonepoet DSD-to-PCM Guidance — Evidence-Based Revision v9

## Executive recommendation

Tonepoet should provide:

1. **Auto / Reference** — a versioned, qualified conversion path that requires no DSP decisions from the user.
2. **Manual / Expert** — a structured workflow system that permits arbitrary SoX-ng, FFmpeg, SSRC, FIR, effect, and encoding stages without silently inserting Auto policy.

For the pinned SoX-ng 14.8.0.1 backend, the qualified native path applies to DSD64, DSD128, and DSD256 DSF/DFF inputs. This release does not natively advertise DSD512 or DSD1024 through those handlers.

---

# 1. Reference SoX-ng processing order

For 88.2 kHz and higher PCM output:

```text
native DSD reader
  → explicit fixed processing headroom
  → rate -u directly to the requested PCM rate
  → source/target-selected linear-phase sinc at the PCM rate
  → PCM-domain effects
  → post-effect peak and true-peak analysis
  → one constant gain restoration and DSD level compensation
  → final quantization and dither/noise shaping
  → lossless encoding/container output
```

For 44.1 and 48 kHz output:

```text
native DSD reader
  → explicit fixed processing headroom
  → one qualified rate -u reconstruction/decimation directly to target rate
  → PCM-domain effects
  → measured constant gain
  → final quantization and dither/noise shaping
```

At these low rates, a separate ultrasonic `sinc` is normally unnecessary because the destination Nyquist limit already defines the transition.

## Why `rate -u` comes before the profile `sinc`

SoX-ng's `rate -u` is already a high-rejection band-limited decimator. It prevents aliasing while converting directly from the native DSD rate to the requested PCM rate.

The later `sinc` has a different job: it narrows the retained PCM spectrum from `rate -u`'s near-Nyquist response to Tonepoet's selected product bandwidth.

Measured against pre-decimation `sinc`:

- steady-state outputs matched at approximately -138 to -180 dBFS RMS, depending on the test;
- finite-file boundary behavior was equal or better;
- the PCM-rate `sinc` required tens of taps rather than thousands;
- long conversions were approximately 31–35% faster.

Therefore the Reference order is:

```text
rate -u → sinc
```

A user may choose another order in Manual mode.

---

# 2. Headroom policy

Do not rely on SoX-ng `-G` when `sinc` is present.

In this release, automatic guard placement can leave `sinc` unprotected because `sinc` is classified as a gain-class effect. Stress fixtures produced clipping with `-G` in both candidate orders.

Use explicit fixed attenuation before the complete DSP chain and do not restore it until all sample-altering effects are complete.

Recommended provisional reserve:

```text
12 dB
```

Rationale:

- evaluated profile FIRs required approximately 5.5–6.3 dB worst-case FIR headroom by coefficient absolute-sum analysis;
- `rate` carries an approximately 3 dB internal headroom estimate;
- 12 dB leaves margin while retaining substantially more than 24 effective bits inside the 32-bit effects representation.

Tonepoet should qualify the minimum safe reserve for every profile and may reduce the provisional value once the composite chain has a proven bound.

After processing, measure the complete continuous programme and apply one constant restoration/compensation gain.

---

# 3. DSD level convention

The native readers apply no level compensation.

Unity reconstruction preserves modulation index:

```text
100% modulation ≈ 0 dBFS
50% modulation  = -6.0206 dBFS
```

Tonepoet should distinguish:

```text
normalization:
  programme-dependent scaling to a peak or loudness target

DSD level compensation:
  fixed convention translation, requested up to +6.0206 dB
```

Reference policy:

```text
requested DSD compensation: +6.0206 dB
recommended ceiling:        -1 dBTP
```

Apply the largest constant gain that satisfies the ceiling. Do not use limiting, compression, soft clipping, or track-by-track gain changes.

---

# 4. Reconstruction-bandwidth policy

Target PCM sample rate and reconstruction bandwidth are related but separate controls.

The target sample rate establishes the maximum representable bandwidth. It does not, by itself, determine how much of the DSD spectrum Auto should retain.

Tonepoet should select a complete reconstruction profile using three constraints:

```text
effective profile =
  narrower of:
    requested or Auto-default reconstruction profile
    qualified source-DSD ceiling
    target-PCM ceiling
```

Consequences:

- a low target rate can force a narrower profile;
- a high target rate never widens a lower-rate DSD source beyond its qualified ceiling;
- choosing 352.8, 384, 705.6, or 768 kHz does not automatically select the widest profile those rates can carry;
- users may explicitly request a wider preservation profile when the source and destination both support it.

## B1 — 44.1 kHz target ceiling

```text
approximately 95% near-Nyquist bandwidth
nominal bandwidth point around 20.95 kHz
integrated rate -u response
```

## B2 — 48 kHz target ceiling

```text
approximately 95% near-Nyquist bandwidth
nominal bandwidth point around 22.8 kHz
integrated rate -u response
```

## B3 — DSD64 Reference ceiling

```text
flat through approximately 25 kHz
transition approximately 25–35 kHz
stopband from approximately 35 kHz
```

B3 remains the DSD64 ceiling even when the user selects 176.4, 192, 352.8, 384, 705.6, or 768 kHz PCM.

## B4 — DSD128 Auto / Reference profile

```text
flat through approximately 30 kHz
transition approximately 30–45 kHz
stopband from approximately 45 kHz
```

A five-modulator DSD128 sweep at 176.4 and 192 kHz found a clear practical knee around this profile. It preserves a measurable bandwidth extension over DSD64 while avoiding the rapid ultrasonic-noise growth seen above approximately 45–50 kHz.

## B4W — optional DSD128 Wideband Preservation profile

```text
flat through approximately 35 kHz
transition approximately 35–50 kHz
stopband from approximately 50 kHz
```

B4W is an explicit preservation choice. It raised median full-band noise by approximately 8.6 dB relative to B4 in the tested DSD128 modulators.

The previously proposed 48–70 kHz DSD128 response is no longer an Auto candidate. It remains available as an aggressive expert profile and retained approximately 30 dB more median full-band ultrasonic noise than B4.

## B5 — DSD256 Auto / Reference profile

```text
flat through approximately 48 kHz
transition approximately 48–70 kHz
stopband from approximately 70 kHz
```

B5 is the recommended quality-oriented DSD256 profile at 176.4, 192, 352.8, 384, or higher PCM rates. Choosing a higher PCM rate does not automatically widen it.

## B6 — optional DSD256 Wideband Preservation profile

```text
provisional passband through approximately 88.2 kHz
transition approximately 88.2–140 kHz
stopband from approximately 140 kHz
```

B6 is not selected merely because the user chooses 352.8 or 384 kHz PCM. It is an explicit wideband-preservation choice that retains substantially more ultrasonic DSD modulator noise. It remains subject to qualification against representative native DSD256 sources and modulators.

## DSD512 and higher

SoX-ng 14.8.0.1's native DSF/DFF handlers do not advertise these source rates. Use a separately qualified backend, such as FFmpeg, and define both its source ceiling and its Auto-default reconstruction profile from measured decoder behavior.

Tonepoet may still support PCM targets of:

```text
44.1, 48, 88.2, 96, 176.4, 192,
352.8, 384, 705.6, and 768 kHz
```

The requested target rate must not imply that the source contains useful bandwidth up to that target's Nyquist frequency.

---

# 5. Profile selection examples

```text
DSD64 → 44.1 kHz:
  B1

DSD64 → 48 kHz:
  B2

DSD64 → 88.2 kHz or above:
  B3
```

```text
DSD128 → 88.2 kHz:
  target-limited profile; B4 does not fully fit below 44.1 kHz Nyquist

DSD128 → 96 kHz:
  intended B4 profile, subject to direct 96 kHz qualification

DSD128 → 176.4 kHz or above, Auto / Reference:
  B4

DSD128 → 176.4 kHz or above, explicit Wideband Preservation:
  B4W
```

```text
DSD256 → 88.2 kHz:
  target-limited profile

DSD256 → 96 kHz:
  intended B4 profile, subject to direct 96 kHz qualification

DSD256 → 176.4 or 192 kHz:
  B5

DSD256 → 352.8 or 384 kHz, Auto / Reference:
  B5

DSD256 → 352.8 or 384 kHz, explicit Wideband Preservation:
  B6
```

The important cases are:

```text
DSD128 → 176.4 or 192 kHz:
  B4, approximately 30–45 kHz

DSD256 → 176.4 kHz:
  B5, approximately 48–70 kHz

DSD256 → 352.8 kHz with ordinary Auto policy:
  also B5; the higher sample rate does not require a wider reconstruction band

DSD256 → 352.8 kHz with explicit wideband preservation:
  B6, with substantially greater retained ultrasonic noise

DSD64 → 176.4 kHz:
  retain B3; do not widen merely because the target rate is high
```

## Controlled DSD128 bandwidth result

A controlled sweep used five deterministic SoX-ng DSD128 modulators at both 176.4 and 192 kHz while varying the reconstruction transition.

```text
25–35 kHz:
  median full-band noise approximately -151.58 dBFS RMS
  worst tested approximately -129.73 dBFS RMS

28–42 kHz:
  median full-band noise approximately -139.74 dBFS RMS
  worst tested approximately -121.80 dBFS RMS

30–45 kHz:
  median full-band noise approximately -134.82 dBFS RMS
  worst tested approximately -118.47 dBFS RMS

35–50 kHz:
  median full-band noise approximately -126.23 dBFS RMS
  worst tested approximately -112.56 dBFS RMS

48–70 kHz:
  median full-band noise approximately -104.60 dBFS RMS
  worst tested approximately -98.30 dBFS RMS
```

Noise below 20 kHz remained effectively unchanged. The widening penalty came from additional ultrasonic DSD-shaped noise.

At a fixed profile, 176.4 and 192 kHz produced effectively identical noise results. A controlled current-profile rerun confirmed that the two physical filter orders differ only at a fixture-dependent numerical floor: approximately -167.85 dBFS RMS for a 1 kHz `sdm-4` DSF and -173.44 dBFS RMS for a zero-input `sdm-4` DSF under the 30–45 kHz Reference profile. These are order-null diagnostics, not stopband-rejection figures.

Therefore:

> DSD128 Auto should use the measured 30–45 kHz knee, not the earlier 48–70 kHz proposal.

## Controlled DSD256 bandwidth result

A controlled test held the source at DSD256 and the PCM rate at 352.8 kHz while changing only the reconstruction transition.

```text
352.8 kHz / 48–70 kHz transition:
  controlled current order null: approximately -180.45 dBFS RMS
  overall output noise in the earlier bandwidth fixture: approximately -153.67 dBFS RMS

352.8 kHz / 70–110 kHz transition:
  overall output noise in the earlier bandwidth fixture: approximately -120.91 dBFS RMS

352.8 kHz / 88.2–140 kHz transition:
  controlled current order null: approximately -147.33 dBFS RMS
  overall output noise in the earlier bandwidth fixture: approximately -102.99 dBFS RMS
```

The corresponding current 176.4 kHz / 48–70 kHz order null was approximately -179.90 dBFS RMS. The earlier bandwidth fixture's overall output noise was approximately -153.66 dBFS RMS.

With the same B5 response, DSD256 → 176.4 kHz and DSD256 → 352.8 kHz were effectively the same conversion represented at different PCM rates:

```text
1 kHz fixture null after 2:1 alignment: approximately -180.39 dBFS RMS
modulator-noise fixture null:             approximately -180.83 dBFS RMS
```

Noise below 48 kHz remained essentially unchanged across the tested 352.8 kHz profiles. The large increase in full-band RMS noise came from ultrasonic DSD modulator noise admitted by the wider B6 response.

Therefore:

> Target sample rate determines what bandwidth can be represented. The reconstruction profile determines what bandwidth Tonepoet actually preserves.

---

# 6. Common response requirements

For the qualified native SoX-ng Reference path:

```text
passband ripple:                    ≤ 0.001 dB
requested sinc stopband attenuation: 180 dB
measured composite rejection target: approximately 180 dB
Auto phase response:                linear
normal sample-rate changes:         one, directly to target PCM rate
final quantization stages:          one
```

Use SoX-ng's maximum qualified reconstruction setting:

```text
sinc -a 180
```

The wording matters:

- **180 dB requested attenuation** is the filter-design setting.
- **Approximately 180 dB measured composite rejection** is the qualification target.
- It is not a claim that the completed PCM file has a −180 dBFS full-band noise floor.
- It is not inferred from a `sinc`/`rate` order-null result.

The shaped DSD noise retained inside the selected passband may be far higher than −180 dBFS. Stopband rejection describes how strongly a coherent unwanted component is suppressed relative to the same component in the passband.

The pinned SoX-ng build reached the following end-to-end coherent rejection at each Reference stopband edge with `sinc -a 180`:

```text
DSD64, 25–35 kHz profile:
  approximately 180.12 dB

DSD128, 30–45 kHz profile:
  approximately 189.03 dB

DSD256, 48–70 kHz profile:
  approximately 201.23 dB
```

The DSD128 and DSD256 figures are at or beyond the practical numerical floor of the signed 32-bit effects interface and should be treated as lower bounds, not literal precision claims.

For DSD64, a direct linear-path sweep across 35–44 kHz and four sine phases measured no less than approximately 183.59 dB rejection. An actual DSF encode/decode test at the 35 kHz stopband edge measured:

```text
sinc -a 140:
  approximately 140.31 dB

sinc -a 160:
  approximately 162.08 dB

sinc -a 180:
  approximately 180.12 dB
```

A separately named compatibility or fallback backend may retain a lower hard minimum, such as 140 dB, but it must not be presented as the same qualified SoX-ng Reference profile.

Measure the complete path. Do not treat a nominal command-line argument, an order null, or a zero-valued output sample as sufficient proof by itself.

---

# 7. Quantization and dither

```text
24-bit integer:
  plain TPDF once at final quantization

16-bit integer:
  qualified output-rate-specific Shibata-family noise-shaped dither once

floating point:
  no dither
```

No sample-altering operation may follow final dither/noise shaping.

A later FFmpeg or other encoder may only package or losslessly encode the already-quantized sample values.

---

# 8. Continuous programmes

Process a continuous source before track splitting:

```text
continuous DSD programme
  → reconstruction/decimation
  → PCM effects
  → measured constant gain
  → final quantization
  → track split
```

This preserves FIR, resampler, and noise-shaper state across gapless boundaries.

---

# 9. Manual workflow system

Manual mode must permit arbitrary structured workflows such as:

```text
FFmpeg decode
  → SoX-ng FIR/effects
  → SSRC resampling and final quantization
  → FFmpeg lossless encoding/muxing
```

Manual mode must not silently add:

- the Reference profile `sinc`;
- `rate -u`;
- libsoxr;
- DSD level compensation;
- headroom restoration;
- TPDF or Shibata;
- normalization or limiting.

Tonepoet may warn about likely aliasing, duplicate resampling, premature quantization, missing headroom, or multiple dither stages. It must not rewrite expert intent.

---

# 10. Backend policy

## Native SoX-ng

Use for qualified uncompressed DSD64/128/256 DSF and DSDIFF inputs.

## FFmpeg

Use for:

- DST decoding;
- source rates or containers unsupported by the native SoX-ng handlers;
- explicitly selected manual workflows;
- final codec/container output.

When FFmpeg decodes DSD/DST, its decoder response is already part of the conversion chain. A later SoX-ng `sinc` cannot replace that initial response.

When FFmpeg uses libsoxr, request and verify it explicitly:

```text
resampler=soxr
precision=28
```

## SSRC

Use as an expert PCM-to-PCM resampler or as the owner of final rate-specific 16-bit noise-shaped quantization. Discover supported dither IDs from the installed binary rather than assuming them.

---

# 11. Qualification requirements

Do not use order nulls as evidence of stopband rejection. Order nulls measure agreement between two implementations of the same transfer function and vary with fixture level, spectrum, profile, and inter-effect quantization. Do not compare rows produced from different fixtures or profiles as though they were DSD-rate quality scores.

Test every supported source/target combination for:

1. reader bit mapping and native sample rate;
2. modulation-index level mapping;
3. effective passband and transition;
4. approximately 180 dB measured coherent composite rejection for the native SoX-ng Reference path;
5. alias products in the retained band;
6. phase and group delay;
7. explicit-headroom sufficiency;
8. effect-boundary clipping;
9. finite-stream start/end behavior;
10. exact output duration;
11. continuous-image split/rejoin equivalence;
12. exactly one final quantization;
13. TPDF statistical behavior;
14. Shibata stability and rate mapping;
15. backend fallback detection;
16. sample-preserving final encoding;
17. deterministic provenance and atomic publication.

---

# 12. Final Auto recommendation

For the pinned SoX-ng release:

```text
explicit provisional 12 dB headroom
  → rate -u directly to target PCM rate
  → selected reconstruction sinc at target rate with -a 180, when applicable
  → target-rate PCM effects
  → programme-wide measurement
  → one constant restoration plus peak-safe DSD compensation
  → one final quantization and dither/noise-shaping operation
  → lossless encoding
```

Profile selection is independent from sample-rate selection except for source and target ceilings:

```text
DSD64 Auto:
  B3, approximately 25–35 kHz, whenever the target can contain it

DSD128 Auto:
  B4, approximately 30–45 kHz, whenever the target can contain it

DSD128 Wideband Preservation:
  B4W, approximately 35–50 kHz, only when explicitly selected

DSD256 Auto:
  B5, approximately 48–70 kHz, at 176.4, 192, 352.8, 384, or higher target rates

DSD256 Wideband Preservation:
  B6, approximately 88.2–140 kHz, only when explicitly selected and supported
```

The DSD128 48–70 kHz proposal is no longer recommended for Auto. Across five tested modulators it retained approximately 30 dB more median full-band ultrasonic noise than the measured 30–45 kHz knee without changing noise below 20 kHz.

This is the evidence-backed default. Pre-decimation `sinc` remains available in Manual mode, but the tests do not support making it the Reference implementation. Likewise, a high PCM target rate remains available without forcing Auto to retain a wider, noisier ultrasonic band.


## Stopband-rejection wording

Tonepoet documentation should say:

> The native SoX-ng Reference path uses a 180 dB requested reconstruction-filter attenuation and is qualified for approximately 180 dB coherent composite stopband rejection.

It should not say:

> All output noise is 180 dB below full scale.

Those statements describe different properties.
