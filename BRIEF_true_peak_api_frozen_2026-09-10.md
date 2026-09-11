# tonepoet-true-peak — crate-only work, public API frozen

## Why this brief exists

Two workstreams now run in parallel. In a separate session the operator is
wiring this crate into tonepoet's conversion pipeline, binding against the
crate's public API as it stands today. In this session the crate's internals
continue to be worked on.

Those two efforts collide if the public API moves.

## The one API constraint

The crate's public API surface does not change. No additions, no removals, no
renames, no signature or field changes, no new or removed enum variants, no
changes to the two cargo features.

`src/lib.rs` is the authority on what that surface is; every module under
`src/` is private, so everything reachable from outside the crate is declared
there. That file, not this brief, is the definition.

Everything behind that surface is free to change: module layout, the scheduler,
pruning, bounds, candidate selection, coefficient tables, the streaming passes,
and the internal representation of any public type.

This is a coordination freeze between two concurrent sessions, not backward
compatibility for released software. The crate has no external users, and no
migration paths, deprecation shims, or version markers are wanted.

## Scope

Only `crates/tonepoet-true-peak`. Nothing in tonepoet itself is in scope.

The crate is standalone: one dependency (`num-complex = "=0.4.6"`), no workspace
inheritance. It compiles and links on its own, outside the tonepoet workspace.

## Measured state, 2026-09-10

Xeon E5-1680 v2 (Ivy Bridge-EP, AVX, no AVX2 or FMA), 120 seconds of 24/192
stereo programme, uninstrumented release benchmark, which is the binding wall
gate.

Absolute wall times on this machine depend on CPU clock state. The governor is
`schedutil`, which ramps clocks under sustained load, so the same binary
measures about 25% faster when the machine is busy than when it is idle, which
is the same thing as idle being about 33% slower. Both conditions are given
because neither alone is representative.

                    idle        under load      target
    fast            2.38        1.78            0.66      not met
    standard        3.27        2.45
    reference       6.58        --
    Reporting4x     0.185       --              (`ReportingPeakMeter`, for scale)

Built instead with rustc 1.80.1, fast measured 1.68 s/min under load.

Fast is therefore between 2.5x and 3.6x over its target depending on build and
conditions, and `fast_wall_target_met` is false in every run. The ratio between
tiers is stable where the absolutes are not: fast/standard is 0.728 idle and
0.727 under load. Fast is now cheaper than Standard, which was not true of the
previous implementation, and roughly 1.6x faster than it. That comparison is
like-for-like: 3.846 to 2.376 s/min idle, and 2.894 to 1.782 s/min under load,
which is 1.62x in both conditions.

Precision, same material, verified on a confirmed-uninstrumented binary:

    fast        point -0.383939284826 dBTP, matching Reference exactly
                certified interval 0.007147213 dB, essentially all of it
                above the point estimate
    standard    point identical, interval 0.000006820 dB
    reference   point identical, interval 0.000000000 dB
    Reporting4x point -0.415699 dBTP, i.e. 0.0318 dB below the true peak

On the crate's own 48 kHz fixture fast's interval is 0.001929187 dB and its
point estimate is again exact. Fast's certified upper bound did not under-read
the true peak on either input.

Where fast's time goes, from a `fast-stage-timing` build, as a share of
attributed time:

    qualified_prefix_block_ingest   45.2%
    hq4_midpoint_survey             28.7%
    flat_envelope                   23.0%
    input_read_decode                2.9%
    candidate_pipeline_inclusive     0.3%   (nomination, proposal, finishing)

The candidate pipeline consumed the previous implementation and is now 0.3% of
runtime; its phase evaluations fell from 11,812,585 to 5,249. The three
whole-signal streaming passes account for 96.9% of attributed time.
`fast_survey_knots` is 184,319,994 on this material, unchanged across both
implementations, and exactly 8.0 knots per input frame.

Compiler auto-vectorization currently contributes nothing measurable. Built from
identical source with the same compiler and only the loop vectorizer toggled,
fast measured 1.683 / 1.683 / 1.678 s/min with it enabled and 1.683 / 1.676 /
1.685 s/min with it disabled, and the two binaries differ by about 4 KB.
Whatever the three streaming passes are bound by on this hardware, it is not
floating-point vector throughput as currently compiled.

## Test state

`cargo test -p tonepoet-true-peak --no-fail-fast` completes with 82 passing and
1 failing:

    lib          58 passed, 1 failed    1212s
    fast_scan    10 passed, 0 failed       3s
    meter        14 passed, 0 failed    1196s

The suite is slow because the Reference tier pays roughly 777 frames of filter
priming per scan, so even short synthetic fixtures take tens of minutes in an
unoptimized test build.

The three defects reported against the previous bundle -- the missing
`HQ1024V1_RECONSTRUCTION_LINF_GAIN_UPPER` import, the orphaned
`update_channel_peaks_scaled` caller, and the stale `1.9638475377212528`
metadata literal -- are all fixed in this source and needed no local patching.

## The failing test

`fast_scan::tests::v2_signed_fits_round_only_bounded_local_offsets_and_respect_ties`
fails at `src/fast_scan.rs:2415` on `assert!(delta > 0.0 && delta <= 0.5)`.

`signed_parabolic_delta(0.8, 1.0, 0.6)` returns -0.16666666666666674. With a
left neighbor of 0.8 and a right neighbor of 0.6 the interpolated vertex lies to
the left of the center sample, so a negative signed offset is the physically
correct result, and transposing the inputs to (0.6, 1.0, 0.8) yields
+0.16666666666666677. The end-to-end point estimates are exact on two
independent inputs, which a sign inversion inside the refinement path would not
produce. The rest of that test's assertions were not reached.

## What the operator can and cannot verify

The operator can compile, run the test suite, and run the release benchmark. The
operator cannot verify claims that are only asserted in prose.

Two environment facts constrain how results are produced here. Release builds of
this crate crash the loop vectorizer in rustc 1.93.1 / LLVM 21.1 in
`LoopVectorizePass::processLoop -> VPlan::execute -> VPIRPhi::execute`. This
reproduces on two separate machines, and at opt-level 3, opt-level 2,
`codegen-units=1`, `--force-vector-width=2`, `--interleave-loops=false` and
`-C target-cpu=native`. The only configuration that builds is
`RUSTFLAGS="-C llvm-args=--vectorize-loops=false"`. The same source builds
cleanly with vectorization enabled on rustc 1.80.1 / LLVM 18, so it is a
toolchain regression rather than a property of this code, and since vectorization
is worth nothing measurable it does not affect the numbers above.

The crate declares `rust-version = "1.93"`. The only constructs found below that
are const `f64::to_bits` and `f64::from_bits` (const-stable in 1.83) and
`Option::is_none_or` (1.82); with those substituted the crate compiles on 1.80.

## The outcome being sought

Fast meets its 0.66 s/min wall gate on the operator's hardware, with the public
API unchanged, and without giving up what the current implementation achieved:
a point estimate matching Reference, a certified interval that does not
under-read the true peak, and a cost below the Standard tier.

Nothing in the candidate pipeline can close the remaining gap, since it is 0.3%
of runtime. The gap lives in the three whole-signal passes.
