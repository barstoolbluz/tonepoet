# Fast066V2 implementation report

Date: 2026-09-09
Scope: `tonepoet-true-peak` crate
Baseline source SHA-256: `604060fce4a9159383e7200cd35af7325a7ddce8f854bb13782bee93ff3e61d0`
Design input: `fast066-v2-handoff-bundle(2).zip`

## Result

This source implements Fast066V2 against the exact corrected V1 baseline named
by the handoff. Reference, Standard, the frozen HQ1024 reconstruction, the
qualified 2x prefix, tile/group geometry, finite-stream policy, and certificate
reduction are unchanged. Fast's optional point-estimation work is replaced with
the bounded 64-nominee / 8-finisher policy.

No production performance claim is made from this execution environment. The
container available for this implementation did not contain a Rust toolchain and
could not reach a Rust distribution or package endpoint, so `cargo test`,
release codegen, and the binding Rust wall-time gate could not be executed here.
The source-round and independent numerical checks that *were* executable are
listed below. This distinction is deliberate.

## Implemented Fast066V2 graph

For each channel/tile the implementation now:

1. executes the existing qualified 2x prefix and complete HQ4 survey;
2. computes the existing outward A4/B4 flat bound for every 256-frame group,
   stores at most 16 group uppers, and still reduces every upper into the final
   certificate;
3. computes the noninitial left-halo quarter-cell bound with the same formula so
   a boundary candidate's complete optional-work domain is covered;
4. compares whole-tile and per-candidate valid uppers only against
   `channel_lower_peaks[channel]` in canonical processing order;
5. nominates at most 64 HQ4 local maxima using the existing deterministic
   spatial-plus-global policy and signed parabolic score;
6. retains each valid fit's bounded integer proposal offset and evaluates or
   reuses exactly one `q0` proposal for each surviving nominee;
7. ranks proposal records by actual evaluated magnitude, then coordinate, then
   nominee coordinate, and admits at most eight finishers;
8. rechecks the candidate's complete-neighborhood upper against the improved
   same-channel lower witness immediately before finishing;
9. evaluates at most `q0-32` and `q0+32`; only a center winner with a valid
   signed concave fit advances to at most `q1-1`, `q1`, and `q1+1`;
10. never widens the search, iterates Newton-style, calls Standard/Reference, or
    falls back to the removed V1 33-knot stencil.

The structural fine-work ceiling is therefore 104 actual newly evaluated
non-HQ4 knots per channel/tile. Survey-knot and same-finisher reuse can only
reduce that count.

## Numerical-envelope optimization

The fine-tail arithmetic graph and frozen coefficients are unchanged. The
implementation derives an outward maximum of all frozen phase L1 bounds once in
`FastMetadata`. While the existing group/halo bounds are built, it also retains
maximum approximate coarse magnitude and maximum qualified-prefix error over the
complete possible fine-work support. These feed one conservative tail-error
envelope per channel/tile.

The hot fine-knot path therefore performs the existing short-tail dot product
without rescanning 34 samples and prefix-error runs for every point. Integer
coarse knots and reused HQ4 survey knots keep their existing tighter error
handling. If the tile-wide common envelope overflows for exceptional finite
magnitudes while a local support can still be represented, the implementation
falls back only to the previous bounded per-knot numerical calculation. This
preserves the supported large-finite behavior without reopening V1 search work
or weakening any outward bound.

## Rust falsification suite

The V2 tests now target the scheduling and bound semantics behaviorally rather
than relying on source-token checks or aggregate multi-tile counters.

Private tests in `src/fast_scan.rs` include:

- `v2_full_tile_capacity_and_work_ceiling_are_per_channel_tile`: pushes enough
  input to retire exactly one nonfinal full mono tile without finalizing the
  second. The deterministic carrier realizes 64 selected nominees, 64 evaluated
  proposals, eight admitted finishers, and **104 actual newly evaluated non-HQ4
  knots** in that single channel/tile. The test asserts those realized counts
  and the 64/8/104 constants directly.
- `v2_rejected_nominee_bound_encloses_every_hq_knot_and_records_same_channel_witness`:
  records rejected-nominee evidence only under `#[cfg(test)]`, independently
  reconstructs the frozen first stage, enumerates every HQ1024 knot in the
  rejected clipped `D_i`, and checks each magnitude against the actual `U_i`
  and the same-channel lower witness used at rejection.
- `v2_cross_group_neighbor_prevents_center_group_only_rejection`: makes the
  center group individually dominated while the adjacent group lifts the full
  neighborhood upper above the witness, then exercises the real nominee probe
  path and proves that the nominee is not rejected.
- `v2_common_tail_envelope_covers_multiple_prefix_error_runs_and_boundary_supports`:
  installs adjacent qualified-prefix blocks with different half-phase error
  envelopes, exercises supports on each side of and across the run boundary for
  every nonzero tail phase, and compares the cached error with the prior local
  formula.
- focused finishing regressions for a missing endpoint, invalid signed
  curvature, a plateau/tie, and a larger +/-32 probe. Each checks actual fine
  evaluation counter deltas so an unbracketed or invalid case cannot silently
  start another search.
- `v2_loud_then_quiet_gates_a_dominated_tile_and_quiet_first_gets_optional_work`:
  exercises both tile-order directions and checks that loud-before-quiet can
  gate a dominated tile while a quiet-first tile still receives bounded
  optional work.

The public integration regression
`fast066_v2_dense_fixture_exercises_dominance_pruning_with_aggregate_sanity_caps`
retains the useful 4,098-frame two-tile fixture for public-diagnostics coverage,
but no longer claims to prove a per-tile quota. The private exact-one-tile test
above is the quota authority.

`tests/meter.rs` also contains
`fast066_pruning_ignores_future_validation_peak_in_the_same_caller_push`. Its
large sample is at frame 7000 in an 8,193-frame carrier, after the first full
tile can retire in one-frame streaming. The first tile is materially quieter.
Whole-push, one-frame, and irregular-push certificates, including diagnostics,
must be identical. This makes accidental use of a caller-push future sample
maximum as the pruning witness observable.

No public audit log or per-tile production telemetry was added. Rejected-nominee
audit state is test-only.

## A/B/C/D commissioning

The normal public constructor still exposes exactly three tiers. The existing
`fast-stage-timing` feature continues to provide only V2 commissioning cuts B,
C, and D inside the crate:

- B: survey + bounds (`fast-survey`);
- C: B + nomination (`fast-nominate`);
- D: complete Fast066V2 (`fast`).

Configuration A is deliberately **not** reintroduced into production code.
`qualification/commission_fast066_v2.py` is an external driver that accepts the
V1 archive separately and requires its SHA-256 to be exactly:

```text
604060fce4a9159383e7200cd35af7325a7ddce8f854bb13782bee93ff3e61d0
```

The driver:

1. hashes the carrier and source inputs before timed execution;
2. extracts and runs A from the unchanged, digest-pinned V1 source;
3. runs B and C from a disposable byte-checked copy of the V2 source;
4. applies the strict C stop/go rule after normalizing to programme duration;
5. if C is at or above 0.66 s/min, records the stop and does not commission D
   for performance purposes;
6. otherwise runs instrumented D for attribution and then uninstrumented D as
   the binding wall-time gate;
7. uses separate V1 and V2 Cargo target directories and records generated
   `Cargo.lock` digests separately from the immutable source-tree identity; and
8. atomically writes one JSON commissioning manifest.

The manifest identifies the A/B/C/D configuration, one carrier SHA-256, sample
rate, channel count, frame count, programme duration, edge policy, V1 archive
and source identity, V2 source identity, algorithm revision reported by each
benchmark, `rustc -vV`, Cargo version, operator `RUSTFLAGS` and relevant Cargo
codegen environment, both crates' release profiles, active SIMD backend,
individual elapsed times, normalized seconds/programme minute, complete raw
benchmark/certificate diagnostics and work counters, optional point-error
statistics, the C stop/go decision, and final speed/accuracy gate state.
Metadata and hashes are collected outside each benchmark executable's measured
wall interval.

`qualification/test_commission_fast066_v2.py` tests the strict normalized C
threshold, result parsing, and A/B/C/D manifest orchestration against one
synthetic carrier digest without pretending to execute Rust.

## Qualification executed here

### Full source-round qualification

Executed:

```text
python3 -B qualification/run_offline_qualification.py
```

Result: **pass**. The runner regenerated and byte-compared all frozen HQ1024 and
qualified-prefix artifacts and regenerated the checked-in Fast066V2 metadata and
verification reports. The V2 source audit reports **37/37** checks true. These
are explicitly source obligations, not evidence that the Rust tests executed.
The runner also executes the commissioning driver's Python unit tests. All
qualification Python modules pass `py_compile`.

The exact-rational Fast flat-bound qualifier evaluated 22,539 dense knots across
zero, constant, alternating, saw, impulse, and deterministic dyadic-random
coarse arrays. All were enclosed. Candidate saturation observed 4096 synthetic
local maxima, retained exactly 64 nominees, and represented all 16 spatial
microgroups.

### Independent V2 handoff numerical probes

The handoff's Python probes were rerun against the frozen coefficient source in
this crate, not merely copied from the handoff.

For the selected 64-nominee / 8-finisher / step-32 policy:

- all 14 base channel-cases matched the enumerated dense finite HQ1024 target to
  floating-point noise;
- worst absolute signed point error was approximately
  `4.821637332766436e-15 dB`;
- the largest observed per-channel/tile fine-query count was 102, below 104;
- all 44 additional synthetic cases likewise matched the dense target to
  floating-point noise;
- worst absolute signed error on that extended set was approximately
  `3.857309866213148e-15 dB`;
- the largest observed extended-case per-tile count was 96.

The independent frozen-coefficient fixture used by the exact-one-tile Rust quota
regression was also checked separately and reaches 64 nominees, 64 proposals,
eight finishers, and exactly 104 fine evaluations for the first tile. The
future-input fixture places a 1.0 sample at frame 7000 while the modeled valid
upper for the first quiet tile is approximately 0.039, so replacing the
same-channel canonical lower witness with a whole-push validation maximum is a
nonvacuous behavioral change.

These are independent floating-model results. They are policy/coefficient and
fixture evidence, not Rust execution or certificate tests.

### Isolated C arithmetic probe

The handoff's C prefix/midpoint microbenchmark was regenerated from the current
crate coefficients, recompiled with `cc -O3 -mavx -ffp-contract=off`, and run for
2000 iterations on this host. The result was:

```json
{"iterations":2000,"prefix_seconds":0.326599520,"midpoint_stereo_seconds":0.085284159,"prefix_s_per_min_192k_stereo":0.282591743,"midpoint_s_per_min_192k_stereo":0.073792513,"roundtrip_max_error":1.1102230246251565e-15}
```

This confirms only that the isolated arithmetic kernels remain inexpensive on
this host. It does not establish Rust scanner throughput and is not the release
gate.

## Required operator gates

Run the complete Rust suite against this exact bundle:

```text
cargo test
cargo test --release
cargo test --features fast-stage-timing
```

Then run the new falsification regressions explicitly so failures are easy to
localize:

```text
cargo test v2_full_tile_capacity_and_work_ceiling_are_per_channel_tile
cargo test v2_rejected_nominee_bound_encloses_every_hq_knot_and_records_same_channel_witness
cargo test v2_cross_group_neighbor_prevents_center_group_only_rejection
cargo test v2_common_tail_envelope_covers_multiple_prefix_error_runs_and_boundary_supports
cargo test v2_finishing_
cargo test v2_loud_then_quiet_gates_a_dominated_tile_and_quiet_first_gets_optional_work
cargo test fast066_pruning_ignores_future_validation_peak_in_the_same_caller_push
```

For commissioning, use the external A/B/C/D driver rather than invoking the V2
cut points manually:

```text
python3 -B qualification/commission_fast066_v2.py \
  --v1-archive /path/to/tonepoet-fast066-v1-implementation-corrected-2026-09-09.tar.gz \
  --carrier /path/to/carrier.f64le \
  --sample-rate 192000 \
  --channels 2 \
  --expected-point-dbtp <revalidated-dense-HQ-point> \
  --rustflags '<operator release flags, including the vectorizer setting>' \
  --output /path/to/fast066-commissioning.json
```

Start with the operator's known-good loop-vectorizer-disabled release setting,
then retry default optimized codegen separately. The driver applies the C
stop/go rule itself. If C is below 0.66 s/min it records both instrumented and
binding D; if not, throughput commissioning stops at C and the mandatory path
should be profiled before further Fast work.

After the retained 192 kHz stereo carrier, run the agreed ordinary/stress corpus
with Reference and Standard baselines unchanged. Release requires both the
uninstrumented full-wall `<= 0.66 s/min` result and the agreed accuracy and
certificate criteria. Until those Rust/operator runs exist, throughput remains
**unverified, not failed**.

## Follow-up inherited regression fixes

A later regression check found three defects inherited from the supplied V1
baseline rather than introduced by Fast066V2. The stale midpoint metadata test
literal now matches the generated metadata authority
(`0x1.f6beb654d7c8cp+0` = `1.9638475377212528`); the certified-scan
test module imports `HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER`; and the
test-only Legacy64 oracle again has its calibrated
`update_channel_peaks_scaled` helper. No Fast066V2 production algorithm or
numerical policy changed.
