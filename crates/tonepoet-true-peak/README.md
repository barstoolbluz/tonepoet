# tonepoet-true-peak

`tonepoet-true-peak` is an application-independent streaming crate for peak
measurement over decoded interleaved `f64` PCM. The certified API exposes
exactly three HQ1024V1 tiers. Standards-compatible reporting remains separate.

## Certified HQ1024V1 measurement

`CertifiedPeakMeter` accepts `PeakTier::Reference`, `PeakTier::Standard`, or
`PeakTier::Fast`.

- `Reference` uses Reference9 and performs exhaustive HQ1024V1 search. It fails
  closed if any live unresolved region remains.
- `Standard` uses Fast90 and returns a deterministic bounded-work certificate.
- `Fast` uses Fast066V2: certified native-input screening followed by selective
  HQ1024V1 reconstruction and deterministic resolution. Its release target is
  0.66 seconds per programme minute.

Changing tier never changes the reconstructed waveform. Every returned
`PeakCertificate` identifies `CertifiedReconstruction::Hq1024V1` and contains
an achieved `[lower, upper]` interval. For a hard-ceiling decision, use the
interval upper rather than the diagnostic point estimate.

```rust
use tonepoet_true_peak::{CertifiedPeakMeter, EdgePolicy, PeakTier};

let mut meter = CertifiedPeakMeter::new(
    48_000,
    2,
    EdgePolicy::RepeatEndpoints,
    PeakTier::Fast,
)?;
meter.push_interleaved(&decoded_frames_a)?;
meter.push_interleaved(&decoded_frames_b)?;
let certificate = meter.finalize()?;
let safe_upper = certificate.finite_interval.upper_linear;
```

All three tiers are deterministic for a fixed backend and invariant to caller
push size. Fast never consults a wall clock while measuring.

## Fast066V2 selective execution policy

`FAST_ALGORITHM_REVISION == "Fast066V2"` remains the frozen public revision
identifier. Fast066V2 retains the frozen HQ1024V1 reconstruction, 4096-interval
canonical tiles, the qualified 8192-point first-stage FFT authority, and the
crate's outward binary64 arithmetic.

The redesigned Fast path does not reconstruct every input interval before it
can reject work. For each channel and canonical tile it instead:

1. reduces exact native-sample maxima and 32-start summaries of native second
   differences over the required 777-frame halo;
2. applies the source-derived conservative raw-domain bound
   `P <= (1 + B_raw) S + A_raw D` first to 256-interval roots and then to
   32-interval children;
3. rejects a group only when its finite qualified upper is no greater than the
   same channel's independently established HQ4 lower witness (`L4`);
4. reconstructs only surviving first-stage half knots, using the symmetric
   1536-tap direct FIR for sparse requests and the existing qualified FFT for a
   sufficiently dense pair-tile request;
5. builds HQ4 samples and the frozen local A4/B4 flat bound only for surviving
   children; and
6. resolves any still-competitive region with deterministic uncapped dyadic
   HQ1024 subdivision until it is dominated or its upper/lower ratio is within
   the private 0.01 dB acceptance criterion.

Raw rejection is deliberately fail-open. A subnormal native support value, an
unusable second-difference rounding enclosure, or another exceptional condition
that prevents a qualified raw upper makes the region survive the screen. Such a
region is never discarded on an uncertain native-domain calculation.

The production resolver has no retired 64-nominee, 8-finisher, or 104-fine-knot
quota and no time cutoff. Parent bounds are replaced by their children as the
dyadic tree is resolved; they do not remain as stale terminal uncertainty. If a
packed dense FFT's numerical enclosure prevents the 0.01 dB criterion on a
competitive span, Fast recomputes that span's required first-stage values with
the channel-local direct authority and rebuilds the local bounds.

The native-screen witness (`L4`) and the full search lower witness (`Lall`) are
kept separately. This is required for sound raw-domain rejection: a finer
HQ1024 point may improve the final lower bound, but it cannot retroactively turn
an HQ4-only raw proof into a different proof.

For ordinary finite input the shipping Fast acceptance gate is a per-channel
certificate width of at most 0.01 dB. Exact digital silence remains `[0,0]`.
Exceptional finite arithmetic can fail closed with `NumericalOverflow`, or
retain unresolved certified coverage as `WorkLimited`; Fast never reports
`TimeLimited`.

`FAST_WALL_NANOS_PER_PROGRAMME_MINUTE == 660_000_000` is an end-to-end release
benchmark target. It is not a runtime deadline and it is not, by itself, a
claim that a particular build or processor achieves the target.

## Sparse/direct and dense/FFT crossover

The implementation uses a fixed pair-tile crossover of 128 requested half
knots. At or below that union size it executes range-local symmetric direct
FIR batches; above it, it feeds the existing qualified FFT a self-contained
finite window `[tile_start-777, tile_end+777]` and retains only requested
outputs. The finite-window adapter resets FFT history before and after each
selective window so unrelated tiles cannot contaminate one another.

The supplied 48 kHz screening fixture supports 128 as a plausible crossover,
but release qualification still requires measuring the permitted fixed
candidate thresholds on the target Xeon E5-1680 v2 system. A screening survivor
fraction or operation-count estimate is not a wall-time result.

## Commissioning compatibility modes

The public commissioning declarations `SurveyBoundsOnly` and `NominationOnly`
are retained. The retired nomination stage no longer exists, so both modes now
stop after native screening, selective first-stage/HQ4 survey, and local flat
bounds. The benchmark reports the latter as `survey-bounds-only-compat` to make
that behavioral equivalence explicit rather than implying nomination work ran.

## Standards-compatible reporting

`ReportingPeakMeter` remains separately exposed. It implements the established
libebur128-compatible 4x/2x/1x reporting profile and is not a lower-accuracy HQ
ceiling tier. At rates below 96 kHz it uses 4x, at 96-191.999 kHz it uses 2x,
and at 192 kHz and above it uses 1x.

## Persistence collision warning

The names `reference`, `standard`, and especially `fast` existed in the retired
Headroom ladder and may occur in application settings. This crate does not
perform persistence migration. An old stored `fast` value meant the retired
16x-prefix job; current `PeakTier::Fast` means Fast066V2 over HQ1024V1. Wider
application code must make that migration explicitly.

## Finite-stream and numerical semantics

For certified HQ measurement:

- non-finite samples are rejected transactionally before streaming state mutates;
- `RepeatEndpoints` and `ZeroExtend` define finite-input extension;
- exact input-sample maxima remain independent lower/report floors;
- channels remain independent and the overall interval encloses their maximum;
- exact digital silence remains an exact zero interval;
- positive subnormals remain distinguishable from silence even under DAZ/FTZ;
- HQ1024V1 retains its frozen induced L-infinity upper of `4.68`; and
- Fast raw storage is bounded by the canonical tile plus the reconstruction
  halo, with local scratch proportional to surviving spans rather than programme
  duration.

## MSRV and release compiler note

The crate MSRV is Rust 1.93. The supplied baseline reports an LLVM 21.1
loop-vectorizer SIGSEGV under default optimized compilation and successful
optimized builds with loop vectorization disabled. Run the established
vectorizer-disabled release configuration first, followed by a separate default
codegen retry. Fast066V2 does not claim to cure that compiler defect.

## Qualification

Run the source-round qualification with:

```text
python3 -B qualification/run_offline_qualification.py
```

It regenerates and byte-compares the frozen HQ1024 and qualified-prefix
artifacts, independently composes all 1,025 raw-screen phases, proves the
checked-in `A_raw`/`B_raw` metadata outward, runs exact raw-screen tests, audits
the selective source graph, and tests the commissioning driver. It does not
compile Rust and it is not a throughput benchmark.

With the shipping Rust toolchain, run at minimum:

```text
RUSTFLAGS='-C llvm-args=--vectorize-loops=false' \
  CARGO_TARGET_DIR=target/fast-shipping \
  cargo test --release --no-fail-fast

RUSTFLAGS='-C llvm-args=--vectorize-loops=false' \
  CARGO_TARGET_DIR=target/fast-timing \
  cargo test --release --features fast-stage-timing --no-fail-fast
```

The Rust suite includes raw-screen all-phase/support-edge checks, raw rejection
against its actual `L4` witness, full-HQ4-oracle comparison after pruning,
scalar/AVX direct-FIR enclosures, sparse/direct versus dense/FFT equivalence,
finite-window FFT boundary checks, DAZ/FTZ regressions, deterministic streaming
and clone invariance, and analytical/fixture accuracy checks.

## A/B/C/D commissioning

`qualification/commission_fast066_v2.py` performs the external comparison
workflow without adding a public tier. It requires the pinned V1 archive and
runs A, the two compatibility ablations B/C, an instrumented D, and the final
uninstrumented binding D when the strict C stop/go threshold permits it. The
driver verifies V1 against SHA-256
`604060fce4a9159383e7200cd35af7325a7ddce8f854bb13782bee93ff3e61d0`;
V1 is extracted into disposable storage and is never copied into the shipping
crate.

```text
python3 -B qualification/commission_fast066_v2.py \
  --v1-archive /path/to/tonepoet-fast066-v1-implementation-corrected-2026-09-09.tar.gz \
  --carrier /path/to/carrier.f64le \
  --sample-rate 192000 \
  --channels 2 \
  --expected-point-dbtp <independently-checked-HQ-target> \
  --rustflags '<operator release flags>' \
  --output /path/to/fast066-commissioning.json
```

The binding release decision records three independent conditions: the 0.66
s/programme-minute wall target, the per-channel 0.01 dB certificate-width gate,
and point error of at most 0.01 dB against the independently checked target.
Carrier/source hashes and compiler metadata are collected outside each timed
benchmark interval. Before timing, the driver performs verbose release builds
and records the actual crate/example `rustc` invocations, including effective
codegen flags. Instrumented V2 and binding D use separate Cargo target
directories. Every run records its A/B/C/D label, carrier digest, edge policy,
source-tree digest, build identity, algorithm revision, active SIMD backend,
elapsed time, certificate/error data, and (for V2) explicit work counters. If C
is at or above 0.66 s/programme-minute, D is not built or run for performance
commissioning and the manifest directs the operator to profile the mandatory
path. Throughput is unverified until this workflow runs on the specified target
processor with the actual two-minute 192 kHz stereo carrier.

`qualification/FAST_INPUT_SCREEN_SPECIFICATION.md` is the specification applied
by this source revision. The inherited `fast066_numerical_probe_report.json`
predates the native-input redesign and is retained only as baseline historical
evidence; it is not a release qualification artifact for this implementation.
