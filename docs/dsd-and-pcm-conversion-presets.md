# PCM to DSD Conversion Presets

## 1. Auto Preset — one-click "best practical" PCM → DSD

| Field to expose | Value / behaviour | Notes |
|-----------------|-------------------|-------|
| Resample engine | SoX native Ultra (rate -u)¹ | Gives ≈165-175 dB stop-band with deterministic two-stage polyphase filter. No external dependencies. |
| Oversample factor | Hide (internal) | Polyphase SRC handles all integer / fractional ratios. |
| Pass-band | Lock to table below | Use SoX's -b (%-of-Nyquist) so you don't surface Hz to the user. Default 99%⁴ |
| Phase response | Linear (fixed) | Auto preset is for offline/high-precision. |
| Kaiser β / taps / transition band / alias flag | Hide | SoX decides. |
| Bit depth | 24-bit fixed (PCM intermediate) | Internal chain runs 32/64-bit float; 24-bit is staging prior to modulation. Ensures noise floor < –140 dBFS before SDM. |
| Output encoding | DSD64/128/256/512/1024 (radio buttons) | Each button maps to a sample-rate and pass-band default (below). |
| Dither / noise-shaping | Hide (N/A) | Dither skipped because ΔΣ modulation overwrites the low bits. If user selects 16-bit, dither toggle is auto-enabled. |

### Pass-band defaults for Auto

| Target DSD rate | PCM oversample Fs (internal) | SoX -b (%) | Pass-band edge (Hz, –0 dB) | Stop-band ≥ (Hz)² |
|-----------------|------------------------------|------------|---------------------|----------------------|
| DSD64 | 352 800 Hz | 99 % | 22 000 | ~22 800 |
| DSD128 | 705 600 Hz | 99 % | 44 000 | ~45 000 |
| DSD256 | 1 411 200 Hz | 99 % | 88 000 | ~90 000 |
| DSD512 | 2 822 400 Hz | 99 % | 160 000 | ~163 000 |
| DSD1024 | 5 644 800 Hz | 99 % | 220 000 (≈0.039 × FsPCM, 39% of Nyquist) | ~225 000 |

¹ If SoX was built with libsoxr, rate -v is marginally steeper/faster; users can override via CLI.
² Stop-band values are approximate. The true knee depends on SoX's exact polynomial design for each ratio; measured stop-band attenuation is ≥−160 dB in Auto mode.
³ Widening Δf modestly at higher Fs lets you keep the 2^18 tap count with ≈−150 dB stop-band. Note that widening Δf also widens phase ripple slightly (<0.005 dB).
⁴ Users can adjust up to 99.5%; SoX cap is 99.7%.

(Pass-band ≈ 0.44 × Fs (half the PCM oversample Fs) keeps ~500 Hz transition, plenty for Auto preset.)

## 2. Sinc Preset — expose the full "Insane" FIR tool-kit

| Field to expose | Default (user may change) | Why / guidance |
|-----------------|---------------------------|----------------|
| Resample engine | upsample L + sinc + rate -I | L = 8 for DSD chain (user may pick 4, 16). |
| Oversample factor | 8× | First multiple that fully bypasses DAC OSF. |
| Filter steepness | Custom | Do not lock—user can choose Steep / Brick-wall. |
| Filter taps | 262,144 (2^18) — fast FFT radix-2 length | User adjustable. Power-of-2 values (e.g., 65,536 to 67,108,864) process 2.5× faster. Auto-updates when Δf slider changes. ~300k ≈ -150 dB; doubling taps → -6 dB better stop-band. |
| Transition band (Hz) (Δf) | 50 | Very narrow; user may widen to save CPU. Auto-tap logic maintains ≈–150 dB stop-band. At 352.8k, Δf/Fs ≈ 1.4×10⁻⁴. |
| Pass-band freq (Hz) | See table below | Hard-coded conservative audio limit. |
| Kaiser β | 16.0 | ≈ −120 dB sidelobes, minimal ripple. |
| Phase response | Linear | User can switch to Minimum if latency matters. Latency: ≈taps ÷ 2 samples at Fs (e.g., 2^18 → 131k samples ≈ 0.37s @ 352.8k). |
| Gain compensation (dB) | Auto-compute: +20 · log₁₀ L dB | +18.06 dB for 8×; +12.04 dB for 4×; +24.08 dB for 16×. I.e. vol 8 or gain +18.06. |
| Allow aliasing | Off | Enable only for creative FX. |
| Bit depth / Sample-rate | Fixed 24-bit / user-selected PCM Fs | These are the PCM staging values before ΔΣ. |
| Output encoding | Same radio list as Auto | Determines final call to dsd64, dsd128, … |

### Conservative pass-band defaults for Sinc

| Target DSD rate | PCM oversample Fs | FIR Pass-band corner | Δf (Hz) | Recommended taps | Remarks |
|-----------------|-------------------|---------------------|------------|------------------|---------|
| DSD64 | 352.8 kHz | 25 kHz | 500 Hz | 262,144 (2^18) | Power-of-2 for 2.5× speed |
| DSD128 | 705.6 kHz | 48 kHz | 1 kHz | 262,144 (2^18) | Power-of-2 for 2.5× speed |
| DSD256 | 1.411 MHz | 96 kHz | 2 kHz | 262,144 (2^18) | Power-of-2 for 2.5× speed |
| DSD512 | 2.822 MHz | 160 kHz | 3 kHz | 262,144 (2^18) | Power-of-2 for 2.5× speed |
| DSD1024 | 5.645 MHz | 224 kHz | 4 kHz | 262,144 (2^18) | Power-of-2 for 2.5× speed |

**Two common strategies:**

| Strategy | Transition spec | How taps scale | Practical default |
|----------|----------------|----------------|-------------------|
| Fixed "audio" knee | Brick-wall at 25k/48k/96k... with constant narrow Δf (e.g., 50 Hz) | Taps rise roughly linearly with target Fs | Use for academic plots or benchmarking |
| **Fixed relative steepness** | Keep Δf ≈ 0.5% of Nyquist (SoX -b 99.0) | Taps hold almost constant across rates | **Good compromise for Auto mode** |

**Recommendation**: Auto mode uses fixed relative steepness for consistent performance across all DSD rates.

## Implementation hints for the agent

### Tap Scaling Strategies

**Key principle**: Filter length ∝ 1 / (normalized transition-band), where normalized transition-band = Δf / Fs

Two approaches for Sinc mode:
1. **Practical (default)**: Scale Δf proportionally with Fs → constant ~300k taps across all rates
2. **Academic**: Fixed Δf in Hz → taps scale linearly with Fs (computationally intensive)

### Preset logic

```pseudo
if direction == "PCM_to_DSD":
    if mode == "Auto":
        use sox rate -u   # native polyphase filter, no manual upsample/sinc
        set -b 99  # via % of Nyquist table above
        # finally: sox dsd{rate}
    else if mode == "Sinc":
        # Note: vol uses linear factor (8 = 8×), gain uses dB (+18.06)
        cmd = f"upsample {L} sinc -{passband} -n {taps} -L -b {beta} vol {L} rate -I"  # linear gain, use gain +20*log10(L) if you expose dB
        # finally: sox dsd{rate}
        
else if direction == "DSD_to_PCM":
    if mode == "Auto":
        use sox rate -u
        set -b per table (95% for ≤352.8k, 97% for ≥352.8k)
        if bit_depth == 16:
            add dither TPDF or Shibata
    else if mode == "Sinc":
        cmd = f"sinc -{passband} -n {taps} -L -b {beta} rate -I"  # no upsample, no gain compensation
        if bit_depth == 16:
            add dither TPDF or Shibata
```

### Gain compensation after zero-insertion upsampling

| Upsample factor L | Linear vol L | gain in dB (20 log₁₀ L) |
|-------------------|--------------|-------------------------:|
| 1× | vol 1 |  +0.00 dB |
| 2× | vol 2 |  +6.02 dB |
| 4× | vol 4 | +12.04 dB |
| 8× | vol 8 | +18.06 dB |
| 16× | vol 16 | +24.08 dB |
| 32× | vol 32 | +30.10 dB |
| 64× | vol 64 | +36.12 dB |

Use either form in SoX:
```bash
# linear factor
... upsample 8 sinc ... vol 8 rate ...
# or dB
... upsample 8 sinc ... gain +18.06 rate ...
```

**Note**: If your FIR is pre-scaled (tap-sum = L), you may disable gain compensation.

- **Dynamic gain field**: auto-compute from table above but allow manual override.
- **Transition band slider**: bound 50 Hz – 5 kHz; recompute taps on change.
- **Tap auto-calc**: 
  - **Practical mode**: Use power-of-2 near 300k (e.g., 262,144 = 2^18)
  - **Academic mode**: For fixed Hz transition: n ≈ 4 / (transition_band / Fs), then round to nearest power-of-2
  - Common power-of-2 values: 65,536 (2^16), 131,072 (2^17), 262,144 (2^18), 524,288 (2^19), 1,048,576 (2^20), 2,097,152 (2^21), 4,194,304 (2^22), up to 67,108,864 (2^26)



Use this table & notes verbatim in your README or feed them to an LLM to generate GUI controls.

---

# DSD to PCM Conversion Presets

## 3. Auto Preset — one-click "best practical" DSD → PCM

| Field to expose | Value / behaviour | Notes |
|-----------------|-------------------|-------|
| Resample engine | SoX native Ultra (rate -u) | Same rationale as PCM → DSD Auto. |
| Target rate table | Lock to table below | Chooses even multiples of 44.1k so shaped noise is out-of-band. |
| Dither | TPDF, no noise-shape (optional toggle) | If exporting 24-bit PCM skip dither; if 16-bit, enable TPDF/Shibata. |

### Default rate map for Auto

| Source DSD | Target PCM | -b | Comment |
|------------|------------|----|---------|
| DSD64 | 88.2 kHz, 24-bit | 95% | Nyquist 44.1k keeps ΔΣ noise ≥20k |
| DSD128 | 176.4 kHz, 24-bit | 95% | Same 2× logic |
| DSD256 | 352.8 kHz, 24-bit | 95% | Plenty of head-room |
| DSD512 | 352.8 kHz (default) / 705.6 kHz | 97% | 352.8k saves space; 705.6k for >20 kHz analysis |
| DSD1024 | 705.6 kHz, 24-bit | 97% | Keeps transition gentle |

## 4. Sinc Preset — expose the full FIR tool-kit for DSD → PCM

| Field to expose | Default (user may change) | Why / guidance |
|-----------------|---------------------------|----------------|
| Resample engine | sinc + rate -I | No upsample needed for decimation |
| Filter steepness | Custom | User can choose Steep/Brick-wall |
| Filter taps | 262,144 (2^18) — fast FFT radix-2 length | Same benefits as PCM → DSD |
| Transition band (Hz) (Δf) | See table below | Scales with target rate |
| Pass-band freq (Hz) | See table below | Conservative audio limit |
| Kaiser β | 16.0 | ≈ −120 dB sidelobes |
| Phase response | Linear | User can switch to Minimum |
| Gain compensation | None | No zero-insertion in decimation |
| Bit depth/dither | Fixed 24-bit (no dither) | If user selects 16-bit, expose TPDF/Shibata options |

### FIR defaults for DSD → PCM Sinc

| Source DSD → Target PCM | Pass-band corner (Hz) | Δf (Hz) | Recommended taps | Notes |
|-------------------------|-----------------------:|--------:|-----------------:|-------|
| DSD64 → 88.2k | 20 000 | 500 | 262,144 | Brick-wall Red Book |
| DSD128 → 176.4k | 20 000 | 500 | 262,144 | Same audio knee |
| DSD256 → 352.8k | 20 000 | 750 | 262,144 | Wider Δf keeps taps flat³ |
| DSD512 → 352.8k | 20 000 | 750 | 262,144 | " |
| DSD1024 → 705.6k | 20 000 | 1 000 | 262,144 | " |

**Note**: Widening Δf modestly at higher Fs lets you keep the 2^18 tap count with ≈−150 dB stop-band.

## Implementation hints for all presets

### Tap Scaling Strategies

**Key principle**: Filter length ∝ 1 / (normalized transition-band), where normalized transition-band = Δf / Fs

Two approaches for Sinc mode:
1. **Practical (default)**: Scale Δf proportionally with Fs → constant ~300k taps across all rates
2. **Academic**: Fixed Δf in Hz → taps scale linearly with Fs (computationally intensive)

### Preset logic

```pseudo
if direction == "PCM_to_DSD":
    if mode == "Auto":
        use sox rate -u   # native polyphase filter, no manual upsample/sinc
        set -b 99  # via % of Nyquist table above
        # finally: sox dsd{rate}
    else if mode == "Sinc":
        # Note: vol uses linear factor (8 = 8×), gain uses dB (+18.06)
        cmd = f"upsample {L} sinc -{passband} -n {taps} -L -b {beta} vol {L} rate -I"  # linear gain, use gain +20*log10(L) if you expose dB
        # finally: sox dsd{rate}
        
else if direction == "DSD_to_PCM":
    if mode == "Auto":
        use sox rate -u
        set -b per table (95% for ≤352.8k, 97% for ≥352.8k)
        if bit_depth == 16:
            add dither TPDF or Shibata
    else if mode == "Sinc":
        cmd = f"sinc -{passband} -n {taps} -L -b {beta} rate -I"  # no upsample, no gain compensation
        if bit_depth == 16:
            add dither TPDF or Shibata
```

### Gain compensation after zero-insertion upsampling

| Upsample factor L | Linear vol L | gain in dB (20 log₁₀ L) |
|-------------------|--------------|------------------------:|
| 1× | vol 1 |   0.00 dB |
| 2× | vol 2 |  +6.02 dB |
| 4× | vol 4 | +12.04 dB |
| 8× | vol 8 | +18.06 dB |
| 16× | vol 16 | +24.08 dB |
| 32× | vol 32 | +30.10 dB |
| 64× | vol 64 | +36.12 dB |

Use either form in SoX:
```bash
# linear factor
... upsample 8 sinc ... vol 8 rate ...
# or dB
... upsample 8 sinc ... gain +18.06 rate ...
```

**Note**: If your FIR is pre-scaled (tap-sum = L), you may disable gain compensation.

- **Dynamic gain field**: auto-compute from table above but allow manual override.
- **Transition band slider**: bound 50 Hz – 5 kHz; recompute taps on change.
- **Tap auto-calc**: 
  - **Practical mode**: Use power-of-2 near 300k (e.g., 262,144 = 2^18)
  - **Academic mode**: For fixed Hz transition: n ≈ 4 / (transition_band / Fs), then round to nearest power-of-2
  - Common power-of-2 values: 65,536 (2^16), 131,072 (2^17), 262,144 (2^18), 524,288 (2^19), 1,048,576 (2^20), 2,097,152 (2^21), 4,194,304 (2^22), up to 67,108,864 (2^26)

**Two common strategies:**

| Strategy | Transition spec | How taps scale | Practical default |
|----------|----------------|----------------|-------------------|
| Fixed "audio" knee | Brick-wall at 25k/48k/96k... with constant narrow Δf (e.g., 50 Hz) | Taps rise roughly linearly with target Fs | Use for academic plots or benchmarking |
| **Fixed relative steepness** | Keep Δf ≈ 0.5% of Nyquist (SoX -b 99.0) | Taps hold almost constant across rates | **Good compromise for Auto mode** |

**Recommendation**: Auto mode uses fixed relative steepness for consistent performance across all DSD rates.

Use this table & notes verbatim in your README or feed them to an LLM to generate GUI controls.
