# Fast066V2 native-input-screen implementation report

Date: 2026-09-10

## Disposition

This tree applies `qualification/FAST_INPUT_SCREEN_SPECIFICATION.md` to the
frozen `tonepoet-true-peak` source bundle. The implementation replaces Fast's
old full-stream reconstruction/fixed-work policy with the specified certified
native-input screen, selective first-stage reconstruction, local HQ4 survey,
and uncapped dyadic completion policy.

The implementation is complete at source level and the offline mathematical
qualification passes. This environment does not contain `cargo`, `rustc`, or
`rustfmt`, so the Rust suite, release build, and target-machine wall-time
commissioning could not be executed here. Those remain mandatory handoff gates;
this report does not treat source inspection or Python qualification as a
substitute for them.

## Inputs and identity

Frozen source archive:

```text
SHA-256  c0f2fd8647c6bd1bc67474bfff6053826fd19531a92d310b6538deaf952a7d07
file     tonepoet-true-peak-api-frozen-2026-09-10.tar(1).gz
```

Design bundle:

```text
SHA-256  d3fd42634f0ecf953d6aced1745ec399caa6bb09d4b8cf7f8cd2c9b193fc65e7
file     fast-input-screen-design(1).zip
```

The embedded specification is byte-identical to the design bundle's
`SPECIFICATION.md`:

```text
SHA-256  8a7968b585aaf99a5ea9ef8d6ebf73e02f10f4fa30dfd33b6f94f6745d6b140f
```

The frozen HQ1024 coefficient source remains byte-identical to baseline:

```text
SHA-256  7070c2e9abc255062dd30aaa516c0827d969d238759e14a61c5d1da94a67de9d
```

`Cargo.toml` and the workspace `Cargo.lock` are byte-identical to baseline.
After comments and the new private `mod raw_screen_metadata;` declaration are
removed, `src/lib.rs` is mechanically identical to baseline. No public type,
method, constant, feature, or dependency was added or changed. In particular:

- `FAST_ALGORITHM_REVISION` remains `Fast066V2`;
- `FAST_WALL_NANOS_PER_PROGRAMME_MINUTE` remains `660_000_000`;
- `PeakTier::Fast.interval_objective_db()` remains `None`;
- the public three-tier constructor surface remains unchanged.

## Implemented execution policy

### Native-domain rejection

`src/fast_scan.rs` now retains the raw PCM needed for canonical 4096-interval
tiles plus a 777-frame halo. Per channel it computes exact sample-magnitude
witnesses and 32-start second-difference summary bins, then applies the frozen
native-domain enclosure

```text
P_g <= S_g + B_raw*S_g + A_raw*D_g
```

with generated private constants:

```text
RAW_A_UPPER             = 0x3ff02862ce0a81a1
RAW_B_UPPER             = 0x3cce9970cbf12e12
FAST_ACCEPT_RATIO_DOWN  = 0x3ff004b7e9b5ce5c
```

The screen uses 256-interval roots followed by 32-interval children. A child
inherits the root curvature upper and recomputes only its endpoint-sample
maximum. Raw rejection is strict against the same-channel `L4` witness; it does
not use tolerance slack or the finer `Lall` witness.

Subnormal raw operands, non-finite second-difference arithmetic, or an unusable
rounding envelope make the cheap screen fail open. They cannot cause a false
raw-domain rejection under DAZ/FTZ behavior.

### Selective first-stage reconstruction

Surviving child supports request native half centers `m=a-8..b+7`. Overlapping
ranges are merged before work is selected.

Sparse ranges use the frozen symmetric 1536-tap first stage directly:

```text
z[2*m+1] = sum(j=0..767)
             h[j] * (x[m+768-j] + x[m-767+j])
```

The implementation has a scalar authority graph and an explicit AVX four-output
batch. Sparse input copies cover only each merged FIR support, not the full tile
halo. AVX capability is dispatched once per meter.

The ordinary direct numerical envelope uses the specified conservative 3072-op
account. The scale-aware exceptional graph has its own 6144-op account.

For dense channel-pair masks, the existing qualified 8192-point executor is
reused through a crate-private finite-window adapter. It resets history before
and after an unrelated finite window, supplies the real absolute input index,
flushes the final partial block, and retains only outputs whose complete FIR
support lies in the supplied `[tile_start-777,tile_end+777]` window. The FFT
plan/executor stays lazy and is never constructed for an all-rejected tile.

The private sparse/dense crossover is currently 128 unique requested half
centers per channel pair. This is a provisional deterministic value supported
by the included screening fixture; the design requires selecting the shipping
value from target-processor measurements at candidate thresholds 32, 64, 128,
256, and 512. No runtime timing/autotuning was introduced.

### Local survey and mandatory completion

Each active span builds only the local coarse support it needs, evaluates the
local frozen HQ4 survey, and applies the existing A4/B4 flat enclosure. The
implementation keeps separate same-channel lower witnesses:

- `L4`: exact native samples plus physically evaluated HQ4 values, used by the
  raw screen;
- `Lall`: all evaluated HQ1024 values, used during local completion.

A competitive flat span enters the frozen dyadic tail hierarchy. Refinement is
a covering partition: splitting a parent removes the parent from unresolved
coverage and replaces it with its two children. The resolver visits the higher
upper first, breaks ties toward the lower HQ coordinate, and has no retired
64-nominee, 8-finisher, or 104-evaluation quota.

Nodes retire only when dominated or when their same-channel upper/lower ratio
meets the private 0.01 dB criterion. Width-one nodes reduce to their endpoint
uppers. Packed-FFT numerical uncertainty that blocks the criterion triggers one
channel-local direct first-stage rescore of that still-competitive span; there
is no repeated direct/FFT oscillation.

The exceptional scale-aware midpoint and tail arithmetic use independent
rounding accounts attached to their actual graphs:

```text
MIDPOINT_EXTREME_ROUNDING_OPS = 64   # actual graph: 61 operations
TAIL_EXTREME_ROUNDING_OPS     = 128  # actual graph: 103 operations
```

These accounts are intentionally distinct from the ordinary 48- and 96-op
envelopes.

### Streaming and certificate reduction

Caller pushes are validated transactionally before measurement state moves.
Input is then committed on canonical 4096-frame boundaries so caller chunking
does not change tile readiness or pruning order. The maximum found while
validating an entire caller push is retained for final sample-peak accounting,
but it is not admitted as a future oracle into the raw-screen witness.

Final per-channel uppers combine surviving terminal coverage with the
independently valid frozen HQ1024 reconstruction norm cap. `Complete` is emitted
only when no terminal unresolved coverage remains above the lower witness;
adequately narrow but still terminal coverage produces `WorkLimited`.
`TimeLimited` is never emitted by this Fast implementation.

Retired nominee/proposal/finisher diagnostics remain part of the frozen public
surface and remain zero.

## Tests and qualification added or revised

Rust regressions were added for the implementation obligations, including:

- exact native-screen constants, all-phase support geometry, strict `L4`
  rejection, and DAZ/FTZ fail-open behavior;
- independent complete HQ4 comparison after pruning;
- scalar/AVX direct-FIR enclosure checks;
- sparse/direct versus dense/FFT reconstruction over the same requested span;
- finite-window FFT retained-edge outputs versus the direct FIR;
- parent-replacement semantics in the dyadic resolver;
- removal of the former 104-evaluation completion limit;
- per-channel 0.01 dB certificate width on hostile inputs;
- independently known 0 dBTP windowed-multitone peak tests;
- exact-tile/FFT-boundary chunk invariance, partial clone behavior, short edge
  chunks, invalid-push transactional behavior, and the future-caller-push
  witness regression;
- silence, signed zero, subnormal, asymmetric stereo, and very large finite
  input behavior.

The positional benchmark keeps its existing interface and now checks the Fast
per-channel 0.01 dB certificate-width gate. The `fast-stage-timing`
commissioning declarations remain frozen. Since the retired nomination stage no
longer exists, `SurveyBoundsOnly` and `NominationOnly` intentionally stop at the
same post-flat-bound point; commissioning labels the latter
`survey-bounds-only-compat` rather than claiming nomination work occurred.

The external commissioning driver implements the required A/B/C/D comparison
without putting V1 back into the crate. It verifies the pinned V1 archive
SHA-256, runs A from that extracted source, runs B/C from the candidate source,
applies the strict C `< 0.66 s/programme-minute` stop/go rule, then runs an
instrumented D and a separately built uninstrumented binding D only when C
passes. Instrumented V2 and binding D use distinct Cargo target directories.
Fresh verbose prebuilds happen outside the benchmark timing boundary and capture
the actual crate/example `rustc` invocations so the manifest records effective
compiler/codegen flags rather than relying only on operator notes. Per-run
records bind the input digest, edge policy, source-tree digest, build identity,
algorithm revision, active SIMD backend, elapsed time, certificate/error data,
and explicit V2 work counters.

## Qualification executed in this environment

The final source-round command was:

```text
python3 -B qualification/run_offline_qualification.py
```

Result: **PASS**.

It regenerated and byte-compared the frozen HQ1024 coefficients, qualified
prefix artifacts, certified-search metadata, Fast metadata, and native-screen
metadata. The exact native-screen suite passed all four tests, including all
1025 frozen native phases, constant/affine identities, support/bin widening,
and outward bounds. The commissioning Python suite passed all nine tests,
including a synthetic strict-threshold stop case and compiler-invocation
provenance parsing.

The selective source audit passes **69/69** obligations. It explicitly covers
the ordinary and exceptional arithmetic accounts, raw fail-open conditions,
strict `L4` pruning, selective direct FIR, lazy finite-window FFT, local HQ4 and
flat bounds, uncapped dyadic completion, public/configuration freeze markers,
streaming invariance hooks, the external A/B/C/D driver/provenance requirements,
and required behavioral Rust regressions. The audit itself is static source
evidence; it does not claim that Rust compiled or ran.

All qualification Python modules pass `py_compile`.

The bundled 48 kHz stereo screening fixture was rerun using the implementation
probe and its in-tree default metadata path. The produced JSON is byte-identical
to `qualification/fixture_48k_screening_implementation.json`. Its workload-only
result is:

```text
native intervals across channels        95,998
root groups                                 376
roots rejected before reconstruction        362
children tested                              112
children rejected                             86
surviving children                            26
surviving native intervals                   832
surviving interval fraction             0.866685%
unique requested half centers              1,120
max half centers in one channel/tile         192
pair tiles dense at threshold 128              1
```

This fixture is not a wall-time benchmark and is not the 192 kHz release
carrier.

## Validation not executable here

This environment has no `cargo`, `rustc`, or `rustfmt`. Consequently, none of
the following is claimed to have run here:

- Rust compilation or formatting;
- `cargo test --release --no-fail-fast`;
- the release benchmark executable;
- scalar/AVX/DAZ/FTZ Rust behavioral tests;
- the actual two-minute 192 kHz screening pilot;
- target-processor sparse/dense crossover tuning;
- the binding 0.66 s/programme-minute wall-time gate.

The source therefore must be compiled and tested before merge. The private
crossover value of 128 must also be confirmed or replaced from measurements on
the shipping target; changing that private constant after commissioning does
not change the public API or proof structure.

## Required handoff commands

From `crates/tonepoet-true-peak` with the intended Rust toolchain:

```sh
python3 -B qualification/run_offline_qualification.py

RUSTFLAGS='-C llvm-args=--vectorize-loops=false' \
CARGO_TARGET_DIR=target/fast-shipping \
cargo test --release --no-fail-fast

RUSTFLAGS='-C llvm-args=--vectorize-loops=false' \
CARGO_TARGET_DIR=target/fast-shipping \
cargo build --release --example bench_ceiling_f64le
```

Run `qualification/screening_probe.py` on the actual 120-second, 192 kHz
carrier, then measure the five required private crossover candidates on the
target processor. Finally run the shipping benchmark/commissioning workflow in
`qualification/README.md` and require both the 0.01 dB accuracy/certificate
criteria and the 0.66 s/programme-minute wall gate.

## Principal modified files

```text
src/fast_scan.rs
src/qualified_half_delay_fft.rs
src/raw_screen_metadata.rs
src/lib.rs                         # private module + documentation only
examples/bench_ceiling_f64le.rs
tests/fast_scan.rs
tests/meter.rs
qualification/FAST_INPUT_SCREEN_SPECIFICATION.md
qualification/generate_raw_screen_metadata.py
qualification/raw_screen_metadata.json
qualification/test_raw_screen.py
qualification/screening_probe.py
qualification/fixture_48k_screening_*.json
qualification/verify_fast_scan.py
qualification/fast066_verification.json
qualification/run_offline_qualification.py
qualification/commission_fast066_v2.py
qualification/test_commission_fast066_v2.py
qualification/README.md
README.md
```

No additional architecture, runtime dependency, public tuning knob, runtime
clock, adaptive policy, or fallback to Standard/Reference was introduced.
