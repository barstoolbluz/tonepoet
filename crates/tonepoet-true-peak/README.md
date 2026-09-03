# tonepoet-true-peak

A small, application-independent streaming true-peak meter for decoded PCM.

The library owns only audio-domain concepts: sample rate, channel count, interleaved decoded `f64` frames, interpolation mode, finite-stream edge policy, level, a band-qualified point-estimate authority, and a separately defined finite Headroom64 ceiling reconstruction. It does not open files, discover tools, know Tonepoet pipeline types, or decide gain policy.

## API shape

```rust
use tonepoet_true_peak::{
    headroom64x_authority, TruePeakConfig, TruePeakMeter, TruePeakMode,
};

let config = TruePeakConfig::new(48_000, 2)
    .with_mode(TruePeakMode::Headroom64x);
let mut meter = TruePeakMeter::new(config)?;
meter.push_interleaved(&decoded_frames_a)?;
meter.push_interleaved(&decoded_frames_b)?;
let point = meter.finalize()?;

// A caller may promote the point estimate to safety authority only when the
// decoded signal path is known to lie inside the qualified frequency domain.
let authority = headroom64x_authority(point.overall, 0.475)?;
```

`push_interleaved` accepts any whole-frame block size. Memory use is bounded by channel count and fixed interpolation state, not programme duration.

## Modes

### `Reporting4x`

`Reporting4x` is the interoperable reporting mode and is unchanged by the Headroom64x work. Its effective profile follows libebur128 1.2.6:

- below 96 kHz: 49-tap Hann-windowed polyphase interpolation at 4x;
- 96 kHz through below 192 kHz: the same 49-tap design at 2x;
- 192 kHz and above: sample peak.

Finite-stream startup/finalization also follows libebur128: zero-initialized delay state, no synthetic pre-roll, and no synthetic flush. Ordinary Rust tests freeze independently established libebur128 1.2.6 reference values with a 0.01 dB compatibility tolerance, so Reporting4x regressions are caught without loading or executing an external meter.

### `Headroom64x`

`Headroom64x` is the high-accuracy point-estimate mode for headroom decisions. It is deliberately separate from the reporting contract.

The interpolation path is a six-stage 2x cascade:

1. 1x -> 2x: exact integer phase plus a 384-tap Type-II equiripple half-sample fractional-delay FIR for the only missing phase. The filter is designed over 0..0.99 of original Nyquist, i.e. 0..0.495 x Fs. Symmetry reduces execution to 192 coefficient products per input frame/channel.
2. 2x -> 4x: 49-tap Blackman-windowed interpolation.
3. 4x -> 8x: 25 taps.
4. 8x -> 16x: 17 taps.
5. 16x -> 32x: 13 taps.
6. 32x -> 64x: 9 taps.

The 64x grid has an analytic worst-case grid miss of `0.002616421594 dB`, versus about `0.041925957 dB` at 16x. The complete 64-phase cascade response audit finds a worst one-sided interpolation deficit of `0.017462966 dB`; adding the analytic grid component and a `0.000010 dB` numerical allowance gives a `0.020089388 dB` component budget. The frozen 4,000-case design search plus a separate two-seed 12,000-case upper-band-biased search found a worst exact-peak under-read of `0.017054299 dB`. The published safety reserve is therefore:

```text
HEADROOM64X_MAX_UNDERREAD_DB = 0.030 dB
```

That leaves about `0.00991 dB` margin over the response-budget calculation and about `0.01295 dB` over the strongest exact-peak counterexample found, while staying well inside the requested `0.05 dB` authority ceiling. The response budget is an engineering qualification bound over the stated band, not a claimed theorem for arbitrary critical-Nyquist content.

The reserve is **not global**. It is qualified only for signals whose maximum frequency is no greater than:

```text
HEADROOM64X_QUALIFIED_MAX_FRACTION_OF_SAMPLE_RATE = 0.495
```

`headroom64x_authority()` requires the caller to declare that signal-band property and returns an error outside the qualified domain. The point estimate itself remains available outside the band. This distinction is intentional: at critical Nyquist, real sample values do not uniquely identify an arbitrary quadrature component, so a finite-sample meter cannot honestly promise a uniform sub-0.05 dB physical true-peak theorem all the way to exactly 0.5 x Fs.

Neither mode clamps input samples to full scale; decoded values above `1.0` can produce positive dBTP.

### Finite ceiling reconstruction

Album-scoped `NormalizePeak` does **not** promote the Headroom64x point estimate with the `0.030 dB` reserve. The production DSD carrier has no fabricated `<= 0.495 * Fs` spectral-support declaration. Instead, `HeadroomCeilingMeter` evaluates a separately named finite waveform contract:

- the signal is each channel of the retained final-rate Float64 PCM carrier;
- production uses `RepeatEndpoints` outside the finite stream (the meter also retains `ZeroExtend` for regression coverage);
- the samples pass through the same six-stage Headroom64 interpolation cascade, but without the point-estimator's `-0.004 dB` calibration;
- over the nominal interval from the first input frame through the last, the continuous reconstruction is straight-line interpolation between adjacent 64x knots;
- channels are independent and the ceiling peak is the maximum absolute reconstructed value over all channels;
- because absolute value on an affine segment is maximized at an endpoint, the continuous peak is bounded by the 64x knot maximum plus the explicit binary64 evaluation enclosure.

This is a mathematical contract for Tonepoet's finite reconstruction model. It is deliberately **not** a claim about arbitrary ideal-sinc/DAC reconstruction or decoded output from a lossy codec. `HEADROOM64X_RECONSTRUCTION_LINF_GAIN_UPPER` separately bounds how a deterministic stored-sample error sequence can amplify in this reconstruction; it is not the published Headroom64x point-estimation reserve.

## Performance

The R3 Headroom16x first stage evaluated a 2001-tap windowed-sinc on every original input frame. Headroom64x instead computes only the missing half-sample phase and exploits coefficient symmetry. The exact coefficient-product count for the R10 generic six-stage implementation is **638 products per original input frame/channel**, not `192 * 64` and not 1022. It is:

```text
stage 1:                    192
stage 2:  2 * (1 + 24)  =    50
stage 3:  4 * (1 + 12)  =    52
stage 4:  8 * (1 +  8)  =    72
stage 5: 16 * (1 +  6)  =   112
stage 6: 32 * (1 +  4)  =   160
                                ---
                                638
```

The optimized exhaustive implementation preserves that mathematical cascade and accumulation order but specializes the exact identity phases, eliminating their coefficient-1 multiplies, and uses doubled circular buffers so every nontrivial FIR history window is contiguous. The resulting count is **576 coefficient products per original input frame/channel** (192 + 48 + 48 + 64 + 96 + 128), while also removing modulo/index adjustment from the FIR inner loops. The latter is expected to matter more than the ~9.7% multiply-count reduction. No adaptive pruning is part of this implementation.

`examples/scan_f64le.rs` reports wall time, realtime factor, and `ns / original frame / channel` for a retained-carrier style scan. Production acceptance numbers must be collected with the shipping Nix/Rust codegen and a clean machine; they are not asserted by unit tests. Reporting4x pays none of the Headroom cascade cost.

## Finite-stream edges

`Headroom64x` retains the selectable `RepeatEndpoints` and `ZeroExtend` finite-stream policies. Measurement is clipped to the nominal input-time interval. `Reporting4x` intentionally ignores those policies and keeps its libebur128-compatible finite-stream semantics.

Both engines are incremental and bounded-state. Feeding identical frames in one block or arbitrary whole-frame chunks produces bit-identical results.

## Correctness and regression protection

Correctness is enforced by ordinary Rust tests, not by an operator commissioning process. The crate-local suite retains EBU Tech 3341 reporting vectors and the historical Headroom64x regressions, including:

- exact aligned 0.30/0.35/0.40 x Fs three-tone (`-6.020599913 dBTP` truth);
- exact aligned 0.4850/0.4875/0.4900/0.4925/0.4950 x Fs five-tone (`0 dBTP` truth);
- the R3 0.4980..0.4988 x Fs enveloped vector, retained explicitly as an **outside-qualified-domain** diagnostic rather than falsely treating it as an in-domain authority case;
- deterministic upper-band frequency/phase and aligned-multitone families, plus frozen strongest cases from the earlier deterministic analytical searches;
- the qualified-domain one-sided under-read reserve and the `0.05 dB` point-estimate target;
- one-shot versus irregularly chunked streaming for both modes, determinism, finite-stream startup/finalization, very short streams, both Headroom64x edge policies, silence/near-silence, multichannel maxima, and above-full-scale input.

The 64x grid constant is tested against its defining formula, `20 * log10(1 / cos(pi / 128))`. A private std-only test also checks the coefficient count and a stable checksum of the exact `f64::to_bits()` values, so an accidental coefficient edit fails normal `cargo test` without introducing a hashing dependency.

Reporting4x compatibility tests contain frozen reference values established independently with libebur128 1.2.6, with provenance comments next to the fixtures. The suite also checks one small, redistributable real-program fixture: a checked-in 48 kHz stereo Float64 PCM excerpt of a genuine saxophone recording. Reporting4x is compared with its frozen libebur128 1.2.6 result within `0.01 dB`, and Headroom64x is compared with a frozen 256x high-quality libsoxr observation within the historical `0.10 dB` independent-reference cross-check tolerance. That libsoxr comparison is a regression/anomaly check, not proof of the `0.030 dB` authority reserve; analytical Headroom64x vectors use closed-form continuous-time truth where available and remain the stronger evidence for that bound. Fixture provenance, license, exact byte hash, derivation, and reference-generation details live in `tests/fixtures/README.md`.

The normal Rust test process reads only the checked-in fixture bytes; it does not load or execute libebur128, FFmpeg, SoX, libsoxr, Python, Nix, network resources, decoders, or real DSD material.

`qualification/design_headroom64_filter.py` is retained only as optional offline developer tooling for intentionally regenerating/auditing the first-stage filter. It requires NumPy/SciPy when run manually, but Cargo, `build.rs`, tests, runtime, and `flake.nix` do not invoke it. It is not a release or commissioning gate.

Run the normal regression suite with:

```sh
cargo test -p tonepoet-true-peak
cargo test --workspace
```
