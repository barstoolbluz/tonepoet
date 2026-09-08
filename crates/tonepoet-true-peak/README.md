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

### PCM Standard, Fast, and Reference scans

`HeadroomScanMode::Standard` is the default. It evaluates the already-qualified 16x mathematical prefix, but executes the expensive 384-tap first-stage half phase with bounded-memory 2048-point overlap-save FFT convolution. Each full block accepts 1665 new original-rate frames, retains only the required 383-frame overlap, and packs two real channels into one complex transform because both channels use the same real impulse response. The integer phase, coefficients, calibration, later three stages, 16x grid, and `<= 0.495 * Fs` qualification domain are unchanged, so the published one-sided bound remains `0.044 dB`. The bound is decomposed as an analytic `0.041090312 dB` grid term, a `0.000145995 dB` worst calibrated-response under-read term, and a `0.000010 dB` numerical allowance. Their `0.041246307 dB` sum leaves about `0.002754 dB` below the declared reserve. The same numerical allowance is also charged to the finite-reconstruction ceiling; it is not consumed silently by the faster executor.

`Headroom64x` remains the explicit `Reference` gold standard at `0.030 dB`. The prior direct-prefix `Fast` (16x, `0.044 dB`) and `Fastest` (8x, `0.084 dB`) tokens remain available for now because existing DSD policy names them; they are no longer the default and can be removed when that separate surface is intentionally simplified. `Reporting4x` remains a different interoperability contract.

The ordinary PCM UI has a separate `Fast` choice implemented by
`HeadroomCeilingMeter::new_accelerated_reference()`. Do not confuse it with the
historical `HeadroomScanMode::Fast` token above. The PCM Fast scanner computes
a Reference-quality 4x prefix everywhere, using the frozen 49-tap second stage
through its symmetric pair-product executor. It screens every 4x interval with
a Bernstein cubic plus an independently qualified finite-operator residual and
selectively evaluates missing 64x knots with the exact composed Reference tail.
Its deterministic refinement cap targets a `0.010 dB` or narrower finite-
Reference search interval; that width is a preferred target rather than a
promised result. If the cap is exhausted, every unrefined interval keeps its
certified upper, so hard-ceiling safety does not depend on meeting the target.
The scanner is intended to be the fastest ordinary-PCM choice, but its
two-minute commissioning goal remains a release benchmark, not a source-code
claim.

The public helpers `headroom16x_authority()` and `headroom8x_authority()` apply only their rung's declared reserve and refuse out-of-domain promotion exactly as `headroom64x_authority()` does.

### Finite ceiling reconstruction

The existing DSD album `NormalizePeak` path does **not** promote any point estimate with its dB reserve. The production DSD carrier has no fabricated `<= 0.495 * Fs` spectral-support declaration. Instead, `HeadroomCeilingMeter` evaluates a separately named finite waveform contract.

The ordinary-PCM true-peak gain step deliberately combines the two ideas rather than conflating them. `Standard` and `Reference` spend their declared `0.044 dB` and `0.030 dB` point reserves as aim-point margins, but do **not** call a band-authority API with invented spectral support; their gain solver receives the maximum of the reserved point and the independent finite-reconstruction upper. PCM `Fast` instead feeds its certified finite-Reference `upper_linear` directly into the signal-authority slot. Its measured point and achieved interval remain diagnostics, not hidden additive reserves. Within the qualified `<= 0.495 * Fs` domain the fixed-rung point reserves retain their published one-sided meaning; outside it, Tonepoet promises only the separately defined finite-reconstruction ceiling, not an arbitrary ideal-sinc/DAC peak.

`HeadroomCeilingMeter` evaluates that finite waveform contract as follows:

- the signal is each channel of the retained final-rate Float64 PCM carrier;
- production uses `RepeatEndpoints` outside the finite stream (the meter also retains `ZeroExtend` for regression coverage);
- the governed reconstruction is always the same uncalibrated full six-stage Headroom64 cascade;
- `Reference` evaluates all 64x knots directly;
- `Standard` evaluates the 16x prefix with the FFT first-stage implementation and uses the same independently qualified 16x-to-64x bridge;
- ordinary-PCM `Fast` evaluates a Reference-quality 4x prefix with its tiny
  paired-stage execution delta covered by the explicit numerical enclosure,
  conservatively screens every interval, and selectively completes candidates
  with the exact 4x-to-64x Reference tail; unresolved screen uppers remain in the final
  certificate when the deterministic refinement budget is exhausted;
- historical `HeadroomScanMode::Fast` evaluates the 16x prefix, bounds its four-point cubic interpolant through Bernstein controls, and adds a conservative `0.0030` induced-L-infinity difference bound to the full 64x reconstruction;
- historical `HeadroomScanMode::Fastest` does the same from the 8x prefix with its own independently recomputed `0.0030` induced-L-infinity difference bound;
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

The reference path is unchanged. The legacy opt-in fast modes stop at a qualified prefix and pair the mathematically symmetric Blackman half-phase taps only in their own execution path:

```text
Headroom64x reference: 576 coefficient products / original frame / channel
Headroom16x fast:      272 coefficient products / original frame / channel
Headroom8x fastest:    240 coefficient products / original frame / channel
```

`Standard` is deliberately not assigned a fake per-frame FIR product count: its dominant first stage is block FFT convolution, so the meaningful metric is measured time per original frame/channel. It amortizes one forward and inverse 2048-point complex FFT across 1665 new frames and, for stereo/even channel counts, across two channels at once. Later 2x stages remain the same direct 16x-prefix implementation.

The direct-FIR counts above are a design model, not a throughput claim. In the production hard-ceiling path, each prefix mode computes only the two interior Bernstein controls of its cubic bridge. `examples/bench_ceiling_f64le.rs` benchmarks the production ceiling meter for `standard`, PCM `accelerated`, `reference`, and the historical `fast`/`fastest` rungs, and reports realtime plus extrapolated single-scan wall minutes for a 40-minute carrier. It deliberately does not label wall time as CPU time. The operator must collect release acceptance numbers with the shipping Nix/Rust codegen on the same material used for comparison; no throughput figure is asserted until that benchmark is run. `examples/scan_f64le.rs` remains available for point-meter profiling.

## Finite-stream edges

All headroom modes retain the selectable `RepeatEndpoints` and `ZeroExtend` finite-stream policies. Measurement is clipped to the nominal input-time interval. `Reporting4x` intentionally ignores those policies and keeps its libebur128-compatible finite-stream semantics.

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
