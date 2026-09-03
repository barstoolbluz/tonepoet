# Tonepoet R10: zero-ceiling + Headroom64x performance implementation report

Date: 2026-09-03

Corrective revision: R2, after review of lossy encoder-negotiated sample-rate behavior.

Starting source: `tonepoet_true_peak_R10_REAL_MATERIAL_CORRECTED_R2_2026-09-02.tar.gz` (the authoritative R10 source contained in the supplied bundle).

## Executive result

This change set implements the zero-ceiling architecture and mechanically optimizes the exhaustive Headroom64x engine without changing its public mode set, qualified coefficient values, Reporting4x behavior, or published Headroom64x point-estimate contract.

The hard-ceiling path no longer gives a raw calibrated Headroom64x point estimate upper-bound semantics. It uses a separately named finite reconstruction bound, resolves the permitted gain in linear amplitude, keeps terminal stored-sample and reconstructed-waveform errors distinct, and converts the proved gain downward into `DbNano` with a directed interval check.

The performance work stops at exhaustive-kernel optimization. No adaptive search/pruning was added. The existing production path already prepares independent track carriers concurrently and applies the repository's existing external-tool concurrency limits. Adding another analysis scheduler without clean production measurements would be speculative. The local execution environment has no Rust toolchain, Nix, Docker, or `perf`, so the required shipping-codegen benchmark/profile matrix could not honestly be run here. Benchmark instrumentation and exactness regressions are included for the Rust-capable handoff environment.

## 1. Exact hard-ceiling waveform contract

### Governed signal

For album-scoped native DSD `NormalizePeak`, measurement operates on the retained final-rate interleaved Float64 PCM carrier produced for each participating DSD track. For lossless output, that is the final stored-PCM rate. For lossy output, it must be a sample rate accepted **directly** by Tonepoet's configured FFmpeg encoder; otherwise the hard-ceiling request is rejected before the carrier is constructed. The final lossy encode explicitly pins that same rate and fails closed if it is missing, mismatched, or unsupported. Thus no sample-rate conversion is permitted after the proved gain.

Channels are independent. The album signal peak is the maximum over channel peaks; a non-first-channel maximum therefore has identical authority to channel 0.

### Finite-stream reconstruction

The ceiling signal is defined by the uncalibrated Headroom64 reconstruction:

1. The finite carrier is extended outside its first/last frame according to the existing `EdgePolicy`; production uses `RepeatEndpoints`.
2. It is passed through the existing six-stage 2x cascade:
   - 384-tap Type-II first stage, symmetry-reduced to 192 coefficient products;
   - 49-tap stage;
   - 25-tap stage;
   - 17-tap stage;
   - 13-tap stage;
   - 9-tap stage.
3. The relevant output interval is the nominal interval from original frame 0 through original frame `N-1`, on the 64x grid. Filter warm-up/flush only supplies the state required to evaluate that finite interval.
4. The Headroom64 point-estimator calibration (`-0.004 dB`) is **not** part of the ceiling reconstruction.
5. Between adjacent 64x reconstruction knots, the continuous waveform is straight-line interpolation. The absolute value of an affine segment reaches its maximum at an endpoint, so the continuous peak under this declared model equals the maximum 64x knot magnitude, apart from the separately bounded binary64 evaluation error.

This is a finite, explicit Tonepoet reconstruction convention. It is not claimed to be an arbitrary analog DAC response, an unspecified ideal sinc reconstruction, or decoded lossy-codec output.

### Numerical enclosure

`HeadroomCeilingMeter` adds a conservative binary64 evaluation allowance of:

`1.0e-11 * input_sample_peak`

to each channel reconstruction maximum. A deliberately pessimistic six-stage Higham-style propagation in the Rust regression remains below `3.4e-12 * input_sample_peak`.

The independent qualification program reproduces the real-program-material reconstruction peak as:

- raw reconstruction peak: `0.9879574206349788`
- verifier upper: `0.9879574206448498`
- numerical enclosure slack: `9.870992911942267e-12` linear FS

The checked-in real fixture SHA-256 remains:

`b6ba8b041ebd87543f04f92267487937128acc9905fc743323567682ef77fd20`

### Relationship to the existing analytical ideal-tone truth

The independent qualification also reports the difference between this finite reconstruction and the separate ideal-tone analytical model used to qualify the Headroom64 point estimator. Those are intentionally reported as model-comparison diagnostics, not silently folded into the ceiling reserve. Examples:

- aligned three-tone, RepeatEndpoints: external-model delta `+0.028431267 dB`;
- upper-band five-tone, RepeatEndpoints: external-model delta `+0.046120032 dB`.

Those numbers mean the two waveform models are not identical. The hard guarantee in this patch is explicitly for the finite reconstruction defined above. No claim is made that this finite verifier is an upper bound on every possible ideal-DAC/sinc model.

## 2. Zero-target semantics and gain proof

Exactly `0.000000000` remains a valid persisted user target. The target is never replaced with an internal effective target or reserve.

For participant `i`, the resolver uses:

- `C`: requested ceiling in linear full-scale amplitude;
- `P_i`: proved finite-reconstruction signal upper bound;
- `E_pre_i`: reconstructed terminal error introduced before fixed gain;
- `E_post_i`: reconstructed terminal error introduced after fixed gain;
- `G`: shared album linear gain.

The proved inequality is:

`G * (P_i + E_pre_i) + E_post_i <= C`

so:

`G <= (C - E_post_i) / (P_i + E_pre_i)`.

Every subtraction, addition, and division at the safety boundary is rounded outward with adjacent-binary64 helpers. The album's shared permitted gain is the minimum of the per-participant permitted gains. This is tighter than combining the largest signal from one track with the largest terminal error from a different track.

The calibrated Headroom64x point (`point_db`) remains available for reporting only and is never relabeled as a signal upper bound.

### Directed `DbNano` conversion

The final permitted linear gain is converted to `DbNano` only downward. The implementation does not depend on `powf`, `exp`, or `log` for the proof step:

- `ln(10)` is enclosed by two neighboring binary64 constants;
- a positive exponential is enclosed with a reduced positive Taylor series and geometric remainder;
- repeated squaring restores the exponent with outward rounding;
- negative dB uses reciprocal interval inversion;
- `log10` is used only to seed the candidate integer nanodecibel value;
- 16 additional nanodecibels are subtracted as an explicit gain-realization guard;
- the candidate is accepted only if the directed *upper* amplitude for that `DbNano` remains `<=` the proved permitted linear gain; otherwise the candidate is decremented and checked again.

At unity, 16 nanodecibels corresponds to about `1.842e-9` relative amplitude. This is negligible compared with ordinary terminal quantization terms while remaining comfortably larger than a few binary64 ulps.

A regression explicitly demonstrates that replacing this resolver with the old `target - raw_point` arithmetic would violate the new zero-ceiling case.

## 3. Terminal realization contract

### Pre-gain carrier term

The retained SoX Float64 analysis carrier represents SoX's signed Q1.31 internal sample as exact power-of-two scaling. Reading the carrier back into the same SoX sample domain therefore introduces no carrier round-trip error:

`E_pre = 0`.

### SoX gain realization

The fixed gain path budgets:

`2^-32 + 2^-51` FS

for SoX Q1.31 nearest realization plus a binary64 coefficient/arithmetic allowance.

### Lossless stored PCM

For lossless output, `AlbumTerminalBound` carries two separate facts:

- `stored_sample_error_linear`: maximum absolute error of a stored PCM sample;
- `post_gain_reconstructed_error_linear`: maximum contribution of the terminal error sequence to the declared reconstructed waveform.

The second is **not** treated as one half-LSB. Production uses the edge-safe bound:

`E_post = E_stored * 4.09`.

The independently derived complete Headroom reconstruction induced L-infinity norm is:

`4.089899431660599`.

The production constant is deliberately widened to `4.09`, leaving `0.000100568339401` absolute norm margin (~`2.46e-5` relative) instead of resting on a one-ulp match to runtime `sin`/`cos` coefficient generation.

`RepeatEndpoints` is important here. An interior LTI convolution of a noise shaper with the reconstruction can be substantially tighter, but it does not by itself prove finite-stream endpoint behavior because endpoint extension also extends the error sequence. The implementation therefore keeps the looser `stored support * reconstruction norm` production proof. The tighter interior figures remain diagnostics only.

### Integer quantization/dither

For no dither, target quantization uses the appropriate half-LSB bound (with the Q1.31 gain realization handled separately).

For SoX TPDF and sloped TPDF, the deterministic stored-sample perturbation support is `1.5` target LSB:

- bounded triangular random term: `< 1.0 LSB`;
- nearest target quantization: `<= 0.5 LSB`.

For classic FIR noise shapers, the stored-sample bound is derived from the pinned SoX-ng recurrence:

`1.5 * (1 + sum(abs(c_j)))` target LSB.

Production stores upward integer ceilings of those derived values. Selected examples from the independent qualification:

- Shibata 44.1 kHz: `83.8857719209` -> `84 LSB`;
- High-Shibata 44.1 kHz: `154.9964651791` -> `155 LSB`;
- Shibata 48 kHz: `49.8673535502` -> `50 LSB`.

Gesemann is IIR, so it is qualified separately through its stable recurrence. The independent bound is:

- 44.1 kHz: `< 21.4951781 LSB` -> production `22 LSB`;
- 48 kHz: `< 19.2232262 LSB` -> production `22 LSB`.

The implementation mirrors SoX-ng's first-filter-within-5%-of-design-rate selection. If no classic shaper matches, SoX falls back to TPDF/sloped TPDF. Consequently the high-rate fallback at 88.2/96/176.4/192/352.8/384 kHz is `1.5 LSB`, not a fabricated 44.1-kHz Shibata reserve.

For dithered hard-ceiling lossless cells, the terminal stage is routed to the SoX implementation whose recurrence is actually bounded. For a lossless format that SoX cannot encode directly (notably ALAC), the planner inserts one SoX-to-WAV gain/dither realization and then performs a sample-preserving lossless final encode. This routing is lossless-only; lossy format routing is unchanged by this rule.

### Floating output

Floating terminal formats do not receive an integer quantization/dither reserve:

- Float32: binary output rounding bound;
- Float64: SoX gain realization where SoX may own the terminal, otherwise the FFmpeg Float64 rounding bound.

### Lossy output

For MP3/AAC/Opus and other lossy output, the authority ends at PCM presented to the encoder. It makes **no** promise about decoded-codec true peak, because lossy encoding can create new peaks.

Corrective R2 closes an encoder-negotiation hole in that contract. The earlier planner rejected only an explicit `PlanOperation::ResamplePcm`; FFmpeg could still insert an encoder-required rate conversion after the `volume` filter when the retained carrier rate was unsupported by the selected encoder. The hard-ceiling path now uses a deterministic table for the encoders Tonepoet actually selects (`libmp3lame`, `libfdk_aac`, `libopus`, `dca`, `ac3`). An unsupported carrier/encoder pair is rejected before DSD analysis. A supported pair is emitted with an explicit `-ar` equal to the measured carrier rate, and the command builder independently rejects a missing/mismatched/unsupported rate.

In particular, `libfdk_aac` direct input rates stop at 96 kHz. An album NormalizePeak request for AAC at 192 kHz therefore now fails closed instead of measuring at 192 kHz and allowing FFmpeg to downsample to 96 kHz after gain. AAC at 96 kHz remains accepted without an additional planned resample. MP3, Opus, DTS, and AC-3 use the same direct-rate invariant against their configured FFmpeg encoders.

Once that invariant holds, the only post-gain terminal transformation inside the lossy authority is sample-format realization at the **same** sample rate. Its deterministic allowance is conservatively bounded using half a signed-16-bit LSB plus binary64 arithmetic, then mapped through the reconstruction norm.

## 4. Tightness

The signal verifier itself is extremely tight under its declared waveform: its only deliberate slack is the `1e-11 * input_peak` numerical enclosure.

Terminal realization can dominate the practical shortfall. For a unity signal and 0 dB ceiling, the following are useful worst-case deterministic terminal-only reserves (they are not Headroom point-estimator reserves):

| terminal case | edge-safe `E_post` (FS) | terminal-only reserve (dB) |
|---|---:|---:|
| Int16, no dither | 6.240939954477e-05 | 0.000542098 |
| Int24, no dither | 2.447352762802e-07 | 0.000002126 |
| Int32, no dither | 9.522791488692e-10 | 0.000000008 |
| Int16, TPDF | 1.872262940760e-04 | 0.001626379 |
| Int24, TPDF | 7.323012705429e-07 | 0.000006361 |
| Int16, Shibata @ 44.1 kHz | 1.048462009290e-02 | 0.091549024 |
| Int24, Shibata @ 44.1 kHz | 4.095649579722e-05 | 0.000355751 |
| Int16, High-Shibata @ 44.1 kHz | 1.934661960462e-02 | 0.169689406 |
| Int24, High-Shibata @ 44.1 kHz | 7.557368138987e-05 | 0.000656449 |
| Int16, Gesemann @ 44.1 kHz | 2.745972631967e-03 | 0.023884023 |
| Int24, Gesemann @ 44.1 kHz | 1.072740415293e-05 | 0.000093178 |
| Float32 | 2.447352762802e-07 | 0.000002126 |
| Float64 | 9.522791488692e-10 | 0.000000008 |
| lossy encoder-input PCM | 6.240844726653e-05 | 0.000542090 |

The large 16-bit aggressive-noise-shaper reserves are not casual conservatism comparable to the old 0.030 dB meter allowance. They are the deterministic support needed to prove a hard ceiling for adversarial bounded dither/noise-shaping error under `RepeatEndpoints`. The default no-dither and ordinary 24-bit cases remain orders of magnitude tighter.

The work order's requested actual/reference *rendered-output* shortfall measurement is not claimed here because the Rust production path could not be executed in this environment. The table above is the proved worst-case terminal budget, not a fabricated render result.

## 5. `0.495 * Fs` and the old 0.030 dB reserve

No `0.475 * Fs` or `0.495 * Fs` spectral-support declaration was added to the DSD production caller.

`HEADROOM64X_MAX_UNDERREAD_DB = 0.030` remains available only under its existing qualified standalone precondition. The hard-ceiling resolver does not call `headroom64x_authority()` and does not promote the calibrated point estimate to an authority.

No R7/R8/R9 0.040 dB chain promotion, commissioning stamp, source-range fingerprint, executable/profile gate, or other attestation mechanism was restored.

## 6. Production gain path and idempotency

The retained carrier is the same final-rate Float64 carrier that the resolved conversion consumes. Runtime album gain planning rejects explicit rate changes, rejects lossy rates that the configured encoder cannot consume directly, pins the supported lossy encoder-input rate on the final FFmpeg command, and makes the command builder fail closed if that invariant is lost.

The submitted-batch barrier:

1. validates a common user target;
2. obtains the terminal bound for each scheduled album/output;
3. pairs every track measurement with that output's terminal bound;
4. resolves one common minimum gain;
5. binds that fixed `DbNano` gain back to each DSD-bearing scheduled album.

The existing scratch retry/rerun path carries the already-resolved runtime gain with the retained carrier. It does not recompute or apply the safety adjustment a second time.

All-silent participation still resolves to exactly `0 dB` gain, provided the terminal process itself fits below the requested ceiling. Positive gain for quiet material, attenuation for hot material, above-full-scale Float64 samples, arbitrary channel count, and non-first-channel maxima remain supported.

## 7. Headroom64x exhaustive-kernel performance work

### Actual arithmetic structure

The starting implementation was not a 192-tap-by-64-phase (`12,288` product) kernel. It was already the six-stage 2x cascade described above.

By source inspection, the generic starting execution evaluates approximately `638` coefficient products per original frame per channel:

`192 + 50 + 52 + 72 + 112 + 160 = 638`.

The optimized exhaustive execution evaluates `576` coefficient products per original frame per channel:

`192 + 48 + 48 + 64 + 96 + 128 = 576`.

The reduction comes entirely from specializing the exact identity phases in the later fixed-2x stages; it does not change qualified filter coefficients or discard any 64x output point.

### Mechanical optimizations

`HeadroomHalfSampleStage` now stores two identical copies of each channel's 384-frame ring. Every required 384-frame history window is therefore contiguous. The 192-product symmetric half-phase loop no longer performs modular indexing per tap.

The five later generic `InterpolatorStage` instances in the Headroom hot path are replaced by private fixed `HeadroomTwoXStage` instances. Each:

- retains the same `build_polyphase_filters()` mathematical coefficients;
- recognizes the exact integer/identity phase;
- performs that phase without a redundant coefficient-1 multiply;
- uses a doubled delay ring for a contiguous nontrivial half-phase dot product;
- retains coefficient order and accumulation order.

Reporting4x continues to use the generic reporting engine and is unchanged.

### Exactness protection

The optimized first stage retains a modulo-indexed reference implementation under `#[cfg(test)]`; deterministic pseudorandom multichannel inputs compare output bit patterns and returned base indices over ring wraparound.

Each later stage is similarly compared against the original generic `InterpolatorStage`, again by exact `f64::to_bits()` equality over deterministic pseudorandom input.

The identity specialization uses `sample + 0.0`, matching the generic `0.0 + sample * 1.0` result for negative zero as well as finite nonzero values.

The existing analytical, upper-band, adversarial, streaming/chunk, coefficient-integrity, and real-program-material meter regressions remain in place. New ceiling tests are additive.

### Coefficient execution layout

The checked-in first-stage coefficient source is unchanged. Its SHA-256 remains:

`4bfaabaf6c9688724e47d187c8c7aa267cae8b58db24a906362c7a81eabfa071`.

Later Headroom stage coefficients continue to be deterministically generated from the same tap/window definitions. The optimized execution representation changes only delay/history layout and identity-phase specialization.

## 8. Production scan topology

The production path creates one retained carrier per participating DSD track. Each carrier is scanned once, sequentially and bounded-memory within its scan worker.

Track preparation is already expressed as independent futures. External reconstruction uses the repository's `ToolConcurrencyLimits`; the default SoX policy is `max(total_cores / 8, 1)` concurrent SoX processes with OpenMP threads divided across that concurrency. Carrier scanning itself is dispatched through the existing Tokio blocking pool after reconstruction for the track. Multiple pre-existing preparation futures may therefore overlap; this work does not add a new unbounded thread/task fan-out or a second analysis concurrency policy.

Because the production topology is not a single forced serial carrier scanner, and because this environment cannot benchmark CPU contention with shipping binaries, no extra track-level analysis scheduler was added.

## 9. Adaptive-search stop gate

No hierarchical/adaptive pruning was implemented.

That is deliberate. The work order requires exhaustive mechanical optimization and sensible production scheduling to be measured first, and allows adaptive complexity only if the clean shipping-codegen result remains materially problematic and adaptive refinement adds a material incremental win. This environment cannot produce those measurements. Shipping an unmeasured pruning algorithm would violate that gate.

The next Rust-capable validation run should benchmark the optimized exhaustive implementation first. Only if it remains inadequate should a separately proved pruning layer be considered.

## 10. Benchmark instrumentation and shipping build facts

`crates/tonepoet-true-peak/examples/scan_f64le.rs` now reports:

- frames;
- sample rate;
- channels;
- wall seconds;
- realtime factor;
- `ns / original frame / channel`;
- measured linear peak and dBTP.

File opening is outside the timed interval; streaming read/meter work remains inside.

Repository build facts found statically:

- workspace `rust-version = "1.82"`;
- `flake.nix` uses `pkgs.rust-bin.stable.latest.default` and `pkgs.rustPlatform.buildRustPackage`;
- no repository `[profile.release]` override was found;
- no repository `RUSTFLAGS`, `target-cpu`, LTO, codegen-unit, or panic-strategy override was found.

The exact stable compiler selected by the shipping Nix lock must be reported by the actual shipping build environment; it is not inferred here.

## 11. Independent qualification executed here

The environment **can** run the two non-shipping development qualification programs and a limited FFmpeg encoder-capability audit.

### Ceiling reconstruction qualification

Command:

```sh
python3 crates/tonepoet-true-peak/qualification/verify_ceiling_contract.py
```

Result: PASS.

It independently reconstructs the cascade with NumPy/SciPy, checks analytical/short-stream frozen values, re-evaluates the checked-in real-program fixture, computes the complete reconstruction norm, and provides high-precision dB-to-linear reference values.

Key result:

- complete polyphase L-infinity maximum: `4.089899431660599` (phase 48);
- production bound: `4.09`.

### Terminal qualification

Command:

```sh
python3 tonepoet-pipeline/qualification/verify_album_ceiling_terminal_bounds.py
```

Result: PASS.

This script is stdlib-only and independently derives the classic FIR support ceilings and Gesemann IIR bounds from the pinned SoX-ng coefficient/recurrence data. It also computes tighter interior LTI shaper+reconstruction diagnostics, but explicitly does not use those diagnostics as production `RepeatEndpoints` authority.

Pinned SoX-ng source revision recorded by the qualification:

`324b8cf873fd7836e8848bd87f7a90d8faa6f849`

### Corrective R2 lossy rate-domain audit

The correction adds ordinary Rust regressions at all three relevant boundaries (carrier-rate admission, topology construction, and final FFmpeg command construction). Those Rust tests are present but, as stated below, could not be executed in this environment.

A direct capability audit of the installed FFmpeg 7.1.5 build confirms the rate tables used for the locally available configured encoders:

- `libmp3lame`: 8/11.025/12/16/22.05/24/32/44.1/48 kHz;
- `libopus`: 8/12/16/24/48 kHz;
- `ac3`: 32/44.1/48 kHz;
- `dca`: 8/11.025/12/16/22.05/24/32/44.1/48 kHz.

That FFmpeg build does not contain `libfdk_aac`, so the AAC integration command cannot be executed locally. The current FFmpeg `libfdk-aacenc.c` encoder definition was independently inspected during correction: it advertises signed-16-bit input and the direct sample-rate set `96/88.2/64/48/44.1/32/24/22.05/16/12/11.025/8 kHz`, with no 192 kHz entry. This matches the corrective table.

As an executable fail-closed check of the command-shaping rule, the installed FFmpeg rejects explicit `-ar 96000` with `libmp3lame` instead of silently selecting another rate. The AAC 192 kHz regression is therefore eliminated earlier by admission; the supplied 42.24 kHz / phase-2.0943951023931953 overshoot vector can no longer reach the encoder under hard-ceiling album NormalizePeak. AAC 96 kHz remains admitted and is explicitly pinned as `-ar 96000`.

## 12. Validation that could not be executed here

The container has:

- Linux x86_64;
- AMD EPYC 9V74 host CPU exposure;
- 5 logical CPUs exposed;
- Python 3.13.5;
- system SoX 14.4.2;
- FFmpeg 7.1.5.

It does **not** contain:

- `cargo`;
- `rustc`;
- `rustfmt`;
- `clippy-driver`;
- Nix;
- Docker;
- `perf`.

The environment also has no usable external package/bootstrap network path. Therefore the following work-order requirements are **not claimed as run**:

```sh
cargo test -p tonepoet-true-peak
cargo test -p tonepoet-pipeline
cargo test --workspace
cargo fmt --check
cargo check --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Also not run here:

- clean starting R10 R2 shipping-codegen baseline;
- optimized exhaustive shipping-codegen benchmark matrix at 44.1/48, 88.2/96, 176.4, 192, 352.8 and 384 kHz as supported;
- CPU profiler capture;
- production retained-carrier wall-time benchmark under simultaneous conversion load;
- Rust mutation execution;
- rendered-output ceiling-shortfall measurement through the complete Rust production path.

This is a hard validation limitation, not a test pass.

## 13. Required Rust-capable handoff validation

Run in the same Nix/shipping environment used for distributed Tonepoet binaries:

```sh
cargo test -p tonepoet-true-peak
cargo test -p tonepoet-pipeline
cargo test --workspace
cargo fmt --check
cargo check --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Then run the Headroom scanner benchmark with representative recorded/program material and a peak-dense/adversarial stream at every product-supported rate in the requested matrix. Record CPU/model/platform, compiler version, full codegen flags/profile, frames, channels, wall time, realtime, CPU utilization, competition/load state, scanner topology, and `ns / original frame / channel`.

Profile the optimized exhaustive binary. If optimized exhaustive + the existing production scheduling is already production-appropriate, stop. Do not add adaptive search merely to satisfy a checklist. If it remains materially too slow, any adaptive layer must separately prove its skip bound and earn a roughly 20-25% clean incremental improvement on representative production work.

For mutation execution, at minimum:

1. temporarily replace the linear hard-ceiling resolver with raw `target - point`; confirm `raw_point_subtraction_mutation_would_break_zero_ceiling` / related regressions fail;
2. bias `HeadroomCeilingMeter` below an independent frozen reconstruction value; confirm bound tests fail;
3. if any future adaptive pruning is introduced, make one prune unsafe and confirm exhaustive differential tests fail.

Revert every mutation before release.

## 14. Preservation statement

This change set is intended to preserve all R10 decisions:

- Reporting4x public behavior unchanged;
- Headroom64x public mode and published accuracy contract unchanged;
- no public 16x/32x modes;
- qualified first-stage coefficient values unchanged;
- real-program fixture bytes unchanged;
- no DSD commissioning stamp/script/corpus/profile gate;
- no runtime commissioning warning/failure;
- no old `-30 dBTP` floor;
- no 0.040 dB commissioned-chain reserve;
- no R8/R9 chain/source fingerprint machinery;
- no Python/build/runtime gate.

Static byte comparison confirms both the checked-in coefficient file and real-program fixture are unchanged from the supplied R10 source.

## 15. Acceptance status

The corrective R2 code changes are complete as a handoff candidate, and both independent mathematical qualification programs pass in this environment. The overall work order is **not honestly READY / release-qualified yet**, because this environment cannot execute Rust or produce the mandated clean shipping-codegen benchmarks/profile evidence.

The remaining acceptance work is empirical/toolchain validation, not a known request for more architecture. In particular, adaptive DSP pruning should not be added unless the shipping benchmark proves it is still necessary.
