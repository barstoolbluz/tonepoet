# tonepoet-true-peak

A small, application-independent streaming true-peak meter for decoded PCM.

The library owns only audio-domain concepts: sample rate, channel count, interleaved decoded `f64` frames, interpolation mode, finite-stream edge policy, level, band-qualified point-estimate authorities, and a separately defined finite Headroom64 ceiling reconstruction. It does not open files, discover tools, know Tonepoet pipeline types, or decide gain policy.

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

None of the modes clamp input samples to full scale; decoded values above `1.0` can produce positive dBTP.

### Opt-in faster headroom scans

The headroom question now has a deliberately small three-rung ladder. `Headroom64x` remains the default and the gold standard. `Headroom16x` and `Headroom8x` are explicit speed opt-ins; `Reporting4x` is not part of this ladder because it remains a different reporting contract.

| scan rung | point mode | qualified one-sided under-read bound | role |
|---|---|---:|---|
| `Reference` | `Headroom64x` | `0.030 dB` | default, unchanged |
| `Fast` | `Headroom16x` | `0.044 dB` | middle-speed opt-in |
| `Fastest` | `Headroom8x` | `0.084 dB` | largest accepted speed/accuracy trade |

Both fast modes reuse prefixes of the frozen Headroom64 cascade and the same `<= 0.495 * Fs` authority domain. `Headroom16x` carries a `+0.007 dB` one-sided calibration and `Headroom8x` carries a `+0.088 dB` calibration so that its coarser grid can still support a useful one-sided under-read contract. The independent qualification program recomputes the exact runtime filters, response and grid components, deterministic exact-peak cases, and the finite-ceiling bridge constants.

The public helpers `headroom16x_authority()` and `headroom8x_authority()` apply only their rung's declared reserve and refuse out-of-domain promotion exactly as `headroom64x_authority()` does. `HeadroomScanMode::default()` is `Reference`.

### Finite ceiling reconstruction

Album-scoped `NormalizePeak` does **not** promote any point estimate with its dB reserve. The production DSD carrier has no fabricated `<= 0.495 * Fs` spectral-support declaration. Instead, `HeadroomCeilingMeter` evaluates a separately named finite waveform contract:

- the signal is each channel of the retained final-rate Float64 PCM carrier;
- production uses `RepeatEndpoints` outside the finite stream (the meter also retains `ZeroExtend` for regression coverage);
- the governed reconstruction is always the same uncalibrated full six-stage Headroom64 cascade;
- `Reference` evaluates all 64x knots directly;
- `Fast` evaluates the 16x prefix, bounds its four-point cubic interpolant through Bernstein controls, and adds a conservative `0.0030` induced-L-infinity difference bound to the full 64x reconstruction;
- `Fastest` does the same from the 8x prefix with its own independently recomputed `0.0030` induced-L-infinity difference bound;
- over the nominal interval from the first input frame through the last, the governed continuous waveform is straight-line interpolation between adjacent full-64x reconstruction knots;
- channels are independent and the ceiling peak is the maximum absolute reconstructed value over all channels.

The fast modes therefore change scan cost and the separately reported point estimate; they do not change the waveform whose hard ceiling is being guaranteed. The bridge constants are induced-norm bounds recomputed from the exact runtime filters, and the implementation also adds an explicit binary64 enclosure.

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

The optimized exhaustive `Headroom64x` implementation preserves that mathematical cascade and accumulation order but specializes the exact identity phases, eliminating their coefficient-1 multiplies, and uses doubled circular buffers so every nontrivial FIR history window is contiguous. The resulting count is **576 coefficient products per original input frame/channel** (192 + 48 + 48 + 64 + 96 + 128), while also removing modulo/index adjustment from the FIR inner loops.

The opt-in fast modes do not alter this reference path. They stop at a qualified prefix and pair the mathematically symmetric Blackman half-phase taps only in their own execution path:

```text
Headroom64x reference: 576 coefficient products / original frame / channel
Headroom16x fast:      272 coefficient products / original frame / channel
Headroom8x fastest:    240 coefficient products / original frame / channel
```

Those FIR counts are a design model, not a throughput claim. In the production hard-ceiling path, each fast mode computes only the two interior Bernstein controls of its cubic bridge. That adds 32 floating multiplies per original frame/channel for `Fast` and 16 for `Fastest`, for static totals of 304 and 256 modeled multiplies respectively versus 576 for `Reference`. Relative to the supplied 12.7 CPU-minute Headroom64x baseline, the multiply model extrapolates to about 6.70 CPU-minutes for `Fast` and 5.64 for `Fastest`; those figures are explicitly designed-for, not measured. Additions, abs/max work, memory traffic, and I/O are deliberately not converted into fake throughput numbers. `examples/bench_ceiling_f64le.rs` benchmarks the exact production ceiling meter for `reference`, `fast`, and `fastest` and reports realtime plus extrapolated single-scan wall minutes for a 40-minute carrier. It deliberately does not label wall time as CPU time. The operator must collect release acceptance CPU numbers with the shipping Nix/Rust codegen and a process CPU timer (for example, user+system time from the platform's `time` utility) on a clean machine. `examples/scan_f64le.rs` remains available for point-meter profiling.

## Finite-stream edges

All three headroom rungs retain the selectable `RepeatEndpoints` and `ZeroExtend` finite-stream policies. Measurement is clipped to the nominal input-time interval. `Reporting4x` intentionally ignores those policies and keeps its libebur128-compatible finite-stream semantics.

All engines are incremental and bounded-state. Feeding identical frames in one block or arbitrary whole-frame chunks produces bit-identical results for a fixed mode.

## Correctness and regression protection

Correctness is enforced by ordinary Rust tests, not by an operator commissioning process. The crate-local suite retains EBU Tech 3341 reporting vectors and the historical Headroom64x regressions, including:

- exact aligned 0.30/0.35/0.40 x Fs three-tone (`-6.020599913 dBTP` truth);
- exact aligned 0.4850/0.4875/0.4900/0.4925/0.4950 x Fs five-tone (`0 dBTP` truth);
- the R3 0.4980..0.4988 x Fs enveloped vector, retained explicitly as an **outside-qualified-domain** diagnostic rather than falsely treating it as an in-domain authority case;
- deterministic upper-band frequency/phase and aligned-multitone families, plus frozen strongest cases from the earlier deterministic analytical searches;
- the qualified-domain one-sided under-read reserve and the `0.05 dB` point-estimate target;
- one-shot versus irregularly chunked streaming for every meter mode, determinism, finite-stream startup/finalization, very short streams, both headroom edge policies, silence/near-silence, multichannel maxima, and above-full-scale input.

The 64x grid constant is tested against its defining formula, `20 * log10(1 / cos(pi / 128))`. A private std-only test also checks the coefficient count and a stable checksum of the exact `f64::to_bits()` values, so an accidental coefficient edit fails normal `cargo test` without introducing a hashing dependency.

Reporting4x compatibility tests contain frozen reference values established independently with libebur128 1.2.6, with provenance comments next to the fixtures. The suite also checks one small, redistributable real-program fixture: a checked-in 48 kHz stereo Float64 PCM excerpt of a genuine saxophone recording. Reporting4x is compared with its frozen libebur128 1.2.6 result within `0.01 dB`, and Headroom64x is compared with a frozen 256x high-quality libsoxr observation within the historical `0.10 dB` independent-reference cross-check tolerance. That libsoxr comparison is a regression/anomaly check, not proof of the `0.030 dB` authority reserve; analytical Headroom64x vectors use closed-form continuous-time truth where available and remain the stronger evidence for that bound. Fixture provenance, license, exact byte hash, derivation, and reference-generation details live in `tests/fixtures/README.md`.

The normal Rust test process reads only the checked-in fixture bytes; it does not load or execute libebur128, FFmpeg, SoX, libsoxr, Python, Nix, network resources, decoders, or real DSD material.

`qualification/design_headroom64_filter.py` is retained only as optional offline developer tooling for intentionally regenerating/auditing the first-stage filter. `qualification/verify_fast_headroom_paths.py` independently qualifies the two fast point paths and their conservative bridge to the unchanged full-64x finite reconstruction. They require NumPy/SciPy when run manually, but Cargo, `build.rs`, tests, runtime, and `flake.nix` do not invoke them. They are not release or commissioning mechanisms; ordinary Rust tests freeze the published constants and runtime behavior.

Run the normal regression suite with:

```sh
cargo test -p tonepoet-true-peak
cargo test --workspace
```
